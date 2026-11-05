# AeroSpace + Ghostty as a pane provider

Facts gathered for #80 (can a pane provider be written as one-shot shell
commands, with each neorex pane a Ghostty window tiled by AeroSpace, on
macOS?). Checked against AeroSpace v0.21.3-Beta (`d56e163`, the installed CLI;
source at that tag, `AS` below) and Ghostty v1.4.0 (`83fb956`, the installed
app; source at that tag, `GH` below). Nothing was launched or measured: the
AeroSpace server was not running on this machine, so only `--help` output and
docs/source were read. Items marked **unconfirmed** have no primary source.

## Opening a Ghostty window running a command (macOS)

- `ghostty +new-window` is "Only supported on GTK" (`GH src/cli/new_window.zig:153`).
  It does nothing for macOS.
- The `ghostty` binary cannot launch the app on macOS; the CLI help says "Use
  `open -na Ghostty.app` instead, or `open -na ghostty.app --args --foo=bar`"
  (`GH src/cli/help.zig:57-59`).
- `open -na Ghostty.app --args -e <cmd> <args>`: `-n` always starts a new
  process, i.e. a second app instance, one per window. `-e` sets
  `initial-command`, disables shell expansion, and sets
  `quit-after-last-window-closed=true` so that instance quits when the command
  exits (`GH src/config/Config.zig:1155-1195`). `--title=...`, `--working-directory=...`
  and other config keys also pass through `--args` (`help.zig:45-59`).
- AppleScript, no second instance: Ghostty 1.3.0+ ships a dictionary
  (`GH macos/Ghostty.sdef`; docs: https://ghostty.org/docs/features/applescript,
  "introduced in Ghostty 1.3.0"). The installed 1.4.0 bundle has
  `Contents/Resources/Ghostty.sdef` and `NSAppleScriptEnabled` (read from
  `/Applications/Ghostty.app/Contents/Info.plist`). It is gated by
  `macos-applescript` (default true, `Config.zig:3592`).
- `new window with configuration cfg` returns the new window. A
  `surface configuration` record has `command`, `initial working directory`,
  `initial input`, `wait after command`, `environment variables`, `font size`
  (`Ghostty.sdef:113-128`, `ScriptSurfaceConfiguration.swift`). It has no
  title field; `name override` on the terminal is writable afterwards
  (`Ghostty.sdef:95`).
  Example: `osascript -e 'tell application "Ghostty" to new window with configuration {command:"neorex pane SOCK ID"}'`
  (the record-literal form is unconfirmed; the docs show `set cfg to new surface configuration` then assign).
- `new window` goes through `TerminalController.newWindow`, which ends with
  `NSApp.activate(ignoringOtherApps: true)` (`GH macos/Sources/Features/Terminal/TerminalController.swift:310`,
  comment: "All new_window actions force our app to be active"). So opening a
  pane steals focus to the new window; there is no background-open flag.
- The AppleScript window `id` is Ghostty's own stable id, not an AeroSpace id
  (`AppDelegate+AppleScript.swift`, `valueInScriptWindows(uniqueID:)`). Terminals
  expose `tty`, `pid` (foreground process) and `working directory`
  (`Ghostty.sdef:98-100`). A command can also `focus`, `close`, `split`, `input text`
  to a terminal by id.
- Latency of `open -na` vs `osascript ... new window`: no primary source
  states it. **Unconfirmed.** (`open -n` also starts a process and loads
  config; AppleScript reuses a running instance.)

## When the window's command exits

- `wait-after-command` defaults to false: the surface closes immediately with
  no confirmation when the command exits (`GH src/Surface.zig:1370-1374`,
  `Config.zig:1354`). With it true the window stays until a keypress.
- Exception: a command that runs for at most `abnormal-command-exit-runtime`
  (250 ms default) is treated as abnormal and an error message is shown
  instead of closing (`Surface.zig` `childExited`; `Config.zig:1356-1363`). On
  macOS any exit code counts, not just non-zero.
- Closing the last surface closes the window. The app keeps running on macOS
  (`quit-after-last-window-closed` is false on macOS, `Config.zig:2637`), except
  for `-e` launches (above). So a pane whose `neorex pane` process exits makes
  its Ghostty window disappear on its own; AeroSpace then re-tiles.

## Terminal features inside Ghostty

- Focus reports (DEC 1004): mode `focus_event` (`GH src/terminal/modes.zig:388`),
  honoured in `src/termio/stream_handler.zig:763`.
- Kitty keyboard protocol: implemented (`src/terminal/Terminal.zig`
  `kitty_keyboard` stack; `src/terminal/kitty/key.zig`).
- Undercurl: SGR `4:3` parses to `.curly` (`src/terminal/sgr.zig:284`).
- Synchronized output (DEC 2026): `synchronized_output` (`modes.zig:402`).
- OSC 2 title: Ghostty sets the window title from it unless a `title` config
  is set, which fixes it (`Config.zig:1529-1541`).

## Matching a Ghostty window to an AeroSpace window id

- AeroSpace ids are its own `window-id`s. `aerospace list-windows --all --json`
  (default format `%{window-id} | %{app-name} | %{window-title}`,
  `AS docs/aerospace-list-windows.adoc`) can add any of: `window-id`,
  `window-title`, `window-is-fullscreen`, `window-parent-container-layout`,
  `app-bundle-id`, `app-name`, `app-pid`, `app-exec-path`, `app-bundle-path`,
  `workspace`, `workspace-is-focused`, `workspace-is-visible`,
  `workspace-root-container-layout`, `monitor-*`
  (`AS Sources/Common/cmdArgs/impl/ListWindowsCmdArgs.swift:178-205`). Filters:
  `--pid`, `--app-bundle-id`, `--workspace`, `--monitor`. Ghostty's bundle id
  is `com.mitchellh.ghostty` (installed `Info.plist`).
- There is no tty, no Ghostty id and no geometry in AeroSpace's output. The
  only join keys are `app-pid` and `window-title`.
- With `open -na` each window is its own process, so `app-pid` identifies the
  window uniquely. How the launcher learns that pid: `open` does not print
  it (**unconfirmed**); the pane process can find its ancestor Ghostty pid
  itself, or the launcher can diff `list-windows` before and after.
- With AppleScript (one shared pid) the key is the title: set a unique one
  (`--title` via `--args`, `name override`, or the pane emitting OSC 2 such as
  `neorex:<pane id>`) and select `window-title`. AeroSpace warns "Some windows
  initialize their title after the window appears" (`AS docs/guide.adoc:587`),
  so the match needs a retry loop.
- `aerospace subscribe window-detected` carries `windowId`, `appBundleId`,
  `appName` but not the title or pid (`AS docs/aerospace-subscribe.adoc`,
  `Sources/AppBundle/model/ServerEvent.swift:36`), so a listener must call
  `list-windows` to resolve it.

## Choosing where a new window tiles

- A new tiling window is inserted into the parent of the most recently
  focused window, right after it (`AS Sources/AppBundle/tree/MacWindow.swift:224-238`,
  `mostRecentWindowRecursive`); on an empty workspace it goes last in the root
  container. So the target is chosen by focusing a sibling before opening.
  Dialogs and popup-type windows go to the floating container instead (`:214-221`).
- Because opening in Ghostty steals focus (above), the order is: focus
  neighbour `W` (`aerospace focus --window-id W`), open the Ghostty window,
  then arrange.
- `aerospace split <horizontal|vertical|opposite> [--window-id]` wraps a window
  in a new container but "has no effect if `enable-normalization-flatten-containers`
  is turned on" (`AS docs/aerospace-split.adoc`). Both normalizations default
  to true (`AS docs/config-examples/default-config.toml:21-22`; `guide.adoc:353-414`).
- `aerospace join-with [--window-id W] <left|down|up|right>` puts the window and
  its nearest neighbour in that direction under a common parent; it works with
  normalization on and is the documented replacement for `split`
  (`AS docs/aerospace-join-with.adoc`).
- A recipe for "new pane N below target T" using only these: focus T, open N,
  `join-with --window-id N up`, then `layout --window-id N v_tiles` if the
  orientation is wrong. (Derived from the docs; not run.)

## Focus, move, resize, close (all one-shot, all accept `--window-id`)

- `aerospace focus --window-id ID` (any workspace) or `focus left|down|up|right`
  (`aerospace focus --help`; `AS Sources/AppBundle/command/impl/FocusCommand.swift:35-39`
  focuses by id via `focusWindow()`). `--dfs-index N`, `dfs-next|prev` also exist.
- `move [--window-id] left|down|up|right`, `swap [--window-id] <dir>|dfs-next|dfs-prev`,
  `move-node-to-workspace [--window-id] NAME`, `layout [--window-id] h_tiles|v_tiles|h_accordion|v_accordion|floating|tiling`,
  `close [--window-id]`, `resize [--window-id] smart|smart-opposite|width|height [+|-]N`
  (absolute if no sign) (all `--help`, `AS docs/aerospace-*.adoc`). Resize is in
  pixels/points as AeroSpace defines it; **unconfirmed** whether it maps to
  terminal cells (it does not: the docs give no cell conversion).
- `aerospace flatten-workspace-tree`, `balance-sizes` reset layout.

## Reading the layout

- No command prints the tree or window geometry. Available: per-window
  `window-parent-container-layout` and the workspace's
  `workspace-root-container-layout`; `list-windows` output is sorted by app name
  then title, not tree order (`AS Sources/AppBundle/command/impl/ListWindowsCommand.swift:50-52`).
  Tree order is reachable only through `focus --dfs-index N` (index into the
  workspace's tiling leaves) and `focus dfs-next|dfs-prev`.
- Consequence: neorex can learn which windows exist, which workspace they are
  on, and each one's parent layout, not who is left of whom or sizes in cells.

## Events and callbacks

- `aerospace subscribe [--all] [--no-send-initial] [events...]` streams JSON
  lines and keeps running. Events: `focus-changed` (`windowId`, `workspace`),
  `focused-monitor-changed`, `focused-workspace-changed`, `mode-changed`,
  `window-detected`, `binding-triggered`. On connect it sends the current state
  unless `--no-send-initial` (`AS docs/aerospace-subscribe.adoc`;
  `AS Sources/AppBundle/model/ServerEvent.swift`; broadcast sites `focus.swift:158-174`,
  `MacWindow.swift:256`). There is no window-closed, window-moved or
  layout-changed event; closure shows up only by diffing `list-windows`.
- `subscribe` is long-running, so it is an optional capability, not one-shot.
  The same stream is available over the unix socket
  `/tmp/bobko.aerospace-${USER}.sock` with 4-byte LE length-prefixed JSON
  frames (`AS docs/guide.adoc:838-1000`).
- Config callbacks: `on-focus-changed` and `on-focused-monitor-changed` (commands
  run with `AEROSPACE_WINDOW_ID` or `AEROSPACE_WORKSPACE` set),
  `exec-on-workspace-change` (`AEROSPACE_FOCUSED_WORKSPACE`, `AEROSPACE_PREV_WORKSPACE`),
  `on-window-detected` (`AS docs/guide.adoc:537-720`). They live in the user's
  `aerospace.toml`, so neorex cannot install one without editing that file.
  `aerospace run-callback on-focus-changed` fires them on demand.
- Ghostty has no stream of window events on macOS beyond what AppleScript can
  be polled for; the pane learns focus from DEC 1004 reports.

## Linux equivalents (brief)

- Ghostty GTK supports `ghostty +new-window [--class=..] [--title=..] [-e cmd]`,
  a one-shot that talks to the running instance over D-Bus, starting Ghostty if
  needed (`GH src/cli/new_window.zig:105-174`, "Available since: 1.2.0"). No
  AppleScript, no `open -na`, no second instance.
- sway/i3: `swaymsg`/`i3-msg` one-shot commands, `-t get_tree` returns the full
  layout tree with geometry, and `subscribe` (`window` event with
  `new`/`close`/`focus`/`move`/`title` changes) streams
  (https://i3wm.org/docs/ipc.html, "window event"). Hyprland: `hyprctl` and socket2
  events `openwindow`, `closewindow`, `activewindowv2`, `movewindow`
  (https://wiki.hypr.land/IPC/). All three offer window-closed events and a
  readable layout that AeroSpace lacks. Their window-id-to-terminal mapping
  was not researched; sway exposes a pid per window in the tree, **unconfirmed
  here**.

## What this means for the pane provider interface

- Every required capability can be a one-shot command on macOS: *open a pane*
  (`open -na Ghostty.app --args -e neorex pane SOCK ID`, or `osascript` `new window`),
  *focus a pane* (`aerospace focus --window-id`), *close a pane*
  (`aerospace close --window-id`), *move/resize/swap* (`--window-id` forms),
  *list panes* (`aerospace list-windows --json`).
- The provider must map neorex's pane id to a provider window id. Neither tool
  hands one back from "open", so the contract should let the open command
  print an id, or let neorex resolve it afterwards by a key the pane itself
  emits (pid or an OSC 2 title like `neorex:<id>`); with retry, since titles
  appear late. Do not assume `open` returns a handle.
- "Open" steals focus and tiles next to the previously focused window, so
  placement should be a separate step after open (focus target first, or
  `join-with` afterwards), and the interface should allow open to take a
  placement hint the provider may ignore.
- Panes disappear when their command exits (Ghostty closes the window; the
  250 ms abnormal-exit exception aside), so "close" can be optional and
  "pane gone" must be detectable: by polling `list` or, for richer providers,
  an event.
- Layout reading is weak: no tree, no geometry, no window-closed event. A
  "read layout" capability must be optional, and AeroSpace's would be limited
  to workspace/parent-layout and DFS order; neorex must not require cell sizes
  or neighbour relations from a provider.
- Live focus and window-detected events (`aerospace subscribe`) fit the
  optional events capability; `exec-on-workspace-change` and friends are
  config-file callbacks, not usable as a one-shot capability.
- Terminal features (focus reports, kitty keyboard, undercurl, sync output) are
  all present in Ghostty; a provider could declare them statically.
