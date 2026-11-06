# Finding a terminal's OS window by its OSC 2 title

Question: can a layout provider (AeroSpace, sway, Hyprland, yabai) find the OS
window of a terminal neopane opened, by matching a unique title neopane wrote
with OSC 2 (`ESC ] 2 ; title BEL`) and retrying for a few seconds?

Sources are upstream source trees read at the commits below (shallow clones of
`main`, so "default" means default at that commit; none was run except
`aerospace --version`, whose server was not running). Paths are repo-relative.

| Project | Commit / version read |
|---|---|
| AeroSpace | 0921fb8 (client installed locally: 0.21.3-Beta) |
| Ghostty | 16f5674 |
| kitty | 056b5d0 |
| WezTerm | e68a8ec (`wezterm-gui` 0.2.x-dev) |
| Alacritty | aecf0f6 (0.18.0-dev) |
| foot | 152487e (meson 1.28.0) |
| tmux | 7f2a35ad (3.8) |
| sway | 1652c54 |
| Hyprland | 0197e57 (VERSION 0.57.0) |
| yabai | dd84572 (v7.1.25) |

## 1. AeroSpace

- **Title is the app's AX title, read live.** `MacWindow.getTitle` calls
  `macApp.getAxTitle`, which does `window.get(Ax.titleAttr)`
  (`Sources/AppBundle/tree/MacWindow.swift:120`, `tree/MacApp.swift:255`);
  `Ax.titleAttr` is `kAXTitleAttribute` (`util/accessibility.swift:193`). It
  runs on the app's AX thread each time. There is no title cache in the model.
- `list-windows` resolves `%{window-title}` only when the format uses it
  (`command/format.swift`, `WindowWithPrefetchedTitle`; lazy since commit
  63e0976, 2026-04-14, so an older release may query titles unconditionally,
  which is only slower). `--json` honours `--format`, so
  `--json --format '%{window-id}%{window-title}'` works. The default format
  already includes the title.
- **Filtering:** only `--workspace`, `--monitor`, `--pid`, `--app-bundle-id`,
  `--focused`, `--all`, `--count` (`docs/aerospace-list-windows.adoc`). There is
  **no title filter**; the caller must list and match client-side
  (`aerospace list-windows --all --app-bundle-id com.mitchellh.ghostty
  --format '%{window-id}|%{window-title}'`). Note `--count` and `--format` are
  mutually exclusive.
- **Only registered windows are listed.** `list-windows` walks
  `Workspace.all ... allInhabitedLeafWindowsRecursive` (the model), then drops
  windows that are not `isBound`. The model is updated by AX observers and by
  `refresh()` (`layout/refresh.swift`), which `getOrRegister`s newly alive
  windows. `Shell.run` calls `refreshModel_nonCancellable` *after* a command,
  not before (`shell/Shell.swift:104`). So a window created milliseconds ago may
  be missing from the first call: retrying is required for that, independent of
  titles.
- **Stale/late titles:** AeroSpace's own guide warns "Some windows initialize
  their title after the window appears" (`docs/guide.adoc:588`). For titles set
  later, the read is live, so staleness is bounded by the app updating its
  `NSWindow.title` (see Ghostty's 75 ms debounce below). I found no AeroSpace
  issue or code path that caches titles. No latency figure is published; AX
  reads block on the target app's main thread, so a hung terminal stalls its
  own windows (the code returns `bugPrompt` on failure of the resolve).
- **Native tabs:** AeroSpace treats a native tab group as one window;
  `%{window-id-native}` changes as tabs come to front
  (`docs/util/interpolation-vars-window.adoc`). The AX title is the front
  tab's, so a pane in a background tab is not findable.
- Requires the AeroSpace server and Accessibility permission (the local server
  was not running, so no live experiment was done).

## 2. Ghostty

- **OSC 2 sets the title by default** on macOS and GTK. `stream_handler.zig`
  `windowTitle` forwards `.set_title` to the surface (`src/termio/stream_handler.zig:1005-1031`).
  An empty title resets to the pwd (or blank) and clears `seen_title`.
- **`title = ...` config (and `--title=`) locks it**: "force the title of the
  window to be this title at all times and Ghostty will ignore any set title
  escape sequences" (`src/config/Config.zig:1520-1541`;
  `Surface.zig:1023` drops `.set_title` when `config.title != null`). A
  neopane-launched terminal given `--title=<unique>` is therefore a robust
  alternative that needs no OSC 2 (but then the title cannot be changed later).
- `window-title-font-family` only changes the font (`Config.zig:2169`); it does
  not affect the title text. `window-subtitle` (GTK only) is separate.
- **Shell integration** `shell-integration-features` has a `title` feature,
  **default true** (`Config.zig:8913`, doc at ~2960): bash/zsh/etc. emit
  `OSC 2` with `\w` at each prompt and with the command line before each
  command (`src/shell-integration/bash/ghostty.bash:170,214`). So a title
  written *before* the user's shell prints its next prompt is overwritten. If
  neopane runs a non-shell command (`command = nvim`) the title survives until
  that program changes it; `no-title` disables the feature.
- Initial title when a command is given: Ghostty sets argv[0] for a direct
  command, nothing for a shell-expanded command (`Surface.zig:782-822`).
- **Which title the window carries**
  - macOS: `BaseTerminalController.focusedSurfaceDidChange` subscribes to the
    **focused** surface's `$title` and assigns `window.title`
    (`macos/Sources/Features/Terminal/BaseTerminalController.swift:872-926`).
    Unfocused splits never reach the window title. Native tabs are separate
    NSWindows, each carrying its own focused surface's title.
  - GTK: the window's `title` is bound to the active tab's title
    (`src/apprt/gtk/class/window.zig:357`, `tab.zig:163`: "usually bound to the
    active surface").
  - Manual overrides win: `titleOverride` (AppleScript, `prompt-title` action)
    pins the title; once set, terminal titles are only remembered
    (`macos/.../SurfaceView_AppKit.swift:601-660`; `BaseTerminalController.swift:918`).
- **Delay:** macOS coalesces title changes with a 75 ms timer
  (`SurfaceView_AppKit.swift:643-652`, "short enough that it still feels
  instant"). The AX attribute then follows `NSWindow.title` immediately. No
  documented delay on GTK.
- Bell can prefix the title with a bell emoji (`bell-features = title`,
  `BaseTerminalController.swift:902`): substring matching still works.

## 3. kitty, WezTerm, Alacritty, foot

| Terminal | OSC 2 sets OS window title by default? | Switch to ignore it |
|---|---|---|
| kitty | Yes: `Window.title_changed` -> `Tab.title_changed` -> `sync_os_window_title` for the **active tab's active window** (`kitty/window.py:1632`, `tabs.py:1632`, `state.c:423`) | None global. `--title`/`set-window-title` (window `override_title`) and `--os-window-title` pin it (`launch.py:73,416`; `window.py:942`). `macos_show_window_title_in` only changes where the title is *drawn*. Titles are `sanitize_title`d |
| WezTerm | Yes, but the OS title is **recomputed from the active pane**: `"[i/n] " + pane.title` when the window has >1 tab, else just the title (`wezterm-gui/src/termwindow/mod.rs:1962-2075`) | Lua event `format-window-title` replaces the text entirely (docs: `docs/config/lua/window-events/format-window-title.md`). Default keeps the pane title as a substring |
| Alacritty | Yes, `window.dynamic_title = true` default (`alacritty/src/config/window.rs:73`, `event.rs:1869`) | `window.dynamic_title = false`. **Also: `--title`/`-T` on the CLI sets `preserve_title` so OSC 2 is ignored for that window** (`window_context.rs:178`) |
| foot | Yes (Wayland xdg-toplevel title) | `locked-title = yes` (`doc/foot.ini.5.scd:433`); `-T`/`--title` sets only the *initial* title (`foot.1.scd:66`) |

Notes: WezTerm's `format-window-title` and a user's `title` locks are the main
user-config hazards; none of these terminals documents a debounce.

## 4. Window managers (one-shot title listing)

- **sway:** `swaymsg -t get_tree` returns nodes whose `name` is the window title
  (`sway/sway-ipc.7.scd:297-318` "window title"; `sway/ipc-json.c:711-713`
  emits `c->title`). `view_update_title` runs from the xdg-shell `set_title`
  listener (`sway/desktop/xdg_shell.c:351`), so it is live. A `window::title`
  event exists (`sway-ipc.7.scd:1673`) for a subscribe-based alternative.
  `app_id`, `pid` are also in the tree.
- **Hyprland:** `hyprctl clients -j` emits `"title"` and `"initialTitle"`
  (`src/ipc/s1/Commands.cpp:473-475`). `title` is the current one; window rules
  match `initialTitle` or `title`. Live, event `windowtitle`/`windowtitlev2` on
  socket2 (not read in source here).
- **yabai:** `yabai -m query --windows` emits `"title"` for each window
  (`src/window.c:483-490`). It is a cached AX string refreshed on the
  `kAXTitleChangedNotification` -> `WINDOW_TITLE_CHANGED` event
  (`src/application.c:17`, `src/event_loop.c:937-939`), so it is
  event-driven, not read per query; short lag possible if the app does not post
  the notification. Signals can also filter on `title=<regex>`
  (`doc/yabai.asciidoc:740`). Queries need no SIP changes (not verified here).
- AeroSpace: see section 1 (no server-side title filter, client-side match).

## 5. Title stack (CSI 22;2 t push / CSI 23;2 t pop)

| Terminal | Support |
|---|---|
| kitty | Yes (`screen.c:3297 screen_manipulate_title_stack`) |
| Alacritty | Yes (`alacritty_terminal/src/term/mod.rs:2235 push_title`) |
| foot | Yes, depth 128 (`csi.c:1405-1423`) |
| tmux | Yes, per pane (`input.c:2194,2205`; `screen_push_title`) |
| Ghostty | **Parsed, then ignored** (`src/terminal/stream.zig:2723`; `termio/stream_handler.zig:383-384` no-op) |
| WezTerm | **Parsed, then ignored** (`term/src/terminalstate/mod.rs:2212-2217`) |

So push/pop cannot be used as a terminal-agnostic way to restore a user's
title after the marker is found.

## 6. Conclusion

**Sound enough as a heuristic, not a guarantee.** Every provider listed
exposes the *current* window title in a one-shot command (sway, Hyprland,
yabai, AeroSpace), and every terminal listed updates the OS title from OSC 2 by
default. The retry loop is justified: AeroSpace may not have registered the
window yet, Ghostty/macOS debounces 75 ms, yabai is event-driven.

Where it fails:

1. **A terminal title is not always the OS title.** Ghostty macOS only uses
   the *focused* split's title; Ghostty/kitty/WezTerm/macOS native tabs show
   only the active tab's. A pane in a background tab or unfocused split cannot
   be found. neopane must open its own window (or make the marker's surface
   the active one).
2. **Config pins the title:** Ghostty `title=`, Alacritty `dynamic_title=false`
   or `-T`, foot `locked-title`, kitty `--title`/override, WezTerm
   `format-window-title`, Ghostty manual title override. OSC 2 is silently
   dropped. Mitigation: when neopane controls launch flags, pass the marker as
   the initial title (`--title`, `-T`, `--title=`, `--os-window-title`) rather
   than OSC 2; Ghostty and Alacritty then lock to it, which is fine for
   finding the window.
3. **Overwrites:** the shell prompt (Ghostty `shell-integration-features =
   title`, default on; zsh/bash prompts and tmux `set-titles`), or the program
   itself (Neovim with `'title'` set; unverified here). The marker must be
   written by the final long-lived process, and be re-emitted if necessary.
   Retry only helps if the marker is still present when the window is polled.
4. **tmux/ssh:** inside tmux, OSC 2 sets the *pane* title; the outer terminal
   title changes only if `set-titles on` and the format contains `#T`
   (`options-table.c:984-994`), else the escape never reaches the OS window.
5. **Visibility and staleness:** AeroSpace lists only windows it has registered
   (miss on first calls); titles show in all providers once the app has applied
   them. A hung app stalls AeroSpace's AX read.
6. **Ambiguity/format:** titles gain prefixes (WezTerm `[1/2]`, Ghostty bell
   emoji, Hyprland/yabai regex needing escapes), so match by substring with a
   random, regex-safe token (e.g. `neopane-<uuid>`), not equality; control
   characters are stripped by kitty, so keep to printable ASCII.
7. **No title stack on Ghostty and WezTerm**, so the marker cannot be popped;
   after matching, neopane should set a human title instead (or leave it).
8. **Privacy:** other clients can see the marker in the window title. It is
   not a secret; use it only to disambiguate.

Better (not in scope for a terminal-agnostic design) correlations exist
where available: process PID (`aerospace --pid`, `swaymsg` `pid`, yabai `pid`,
Hyprland `pid`), kitty `kitten @ ls`, or terminal-provided window IDs
(`WINDOWID`, `$KITTY_WINDOW_ID`). A title match is a reasonable fallback that
works across every terminal, and the PID of the terminal process (when the
terminal provider launches one process per window) is a stronger primary key.
