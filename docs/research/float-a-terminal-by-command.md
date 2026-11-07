# Floating an already-open terminal window by id, from a one-shot command

Question: can a layout provider run a short-lived command that floats an open terminal window (found by id) at given bounds, and can the window be recognised by title at map time instead?

Researched 2026-11-07. Versions current at that date: sway 1.12 (2026-05-25), Hyprland 0.57.1 (2026-11-03), AeroSpace 0.21.3-Beta (2026-07-16), tmux 3.8 (2026-09-09), Ghostty `main` (tag v1.4.0 exists). "Unconfirmed" means I found no primary source that settles it.

## Summary

| Tool | Float by id? | Size + position absolute, one call? | Geometry readable? | Rule-at-map by title? |
|---|---|---|---|---|
| sway 1.12 | Yes: `[con_id=N] floating enable` | Yes: comma-chained in one `swaymsg`. Pixels (ppt for size/relative move). Position is workspace-relative; `absolute` is relative to all outputs (global) | Yes: `swaymsg -t get_tree`, `rect` in px, absolute (global) | Yes: `for_window [title=…] floating enable` runs in `view_map`, synchronously after the tiled insert |
| Hyprland 0.57.1 | Yes (`address:0x…` selector) | Yes via `hyprctl --batch` (`;`-separated). Absolute move is in global layout coords. Syntax is Lua since 0.55/0.57 | Yes: `hyprctl -j clients`, `at` / `size` (px) | Yes: window rule, evaluated once at open, matches *initial* title; may also set `float`, `move`, `size` |
| AeroSpace 0.21.3-Beta | Yes: `aerospace layout --window-id N floating` | **No.** No absolute move/size command for floating windows; `resize` is tiling-only (issue #9 open); `move` is directional only (PR #2248 open) | **No** window geometry in any interpolation variable | Partly: `on-window-detected` with `if = 'test %{window-title} ~= …'`, but title may not be set yet at detection |
| tmux 3.8 | New panes only via `new-pane`; **existing** panes via `break-pane -W` | Yes: `-x -y -X -Y` in cells or `%` | Not checked (out of scope) | n/a (tmux owns the pane) |

Ghostty: a title given at launch is applied in `Surface.init` and then fixed; OSC 0/2 is ignored while it is set (confirmed in source). Whether a compositor sees it before the first map is unconfirmed.

## 1. sway (1.12)

Sources: [sway(5)](https://github.com/swaywm/sway/blob/1.12/sway/sway.5.scd), [sway-ipc(7)](https://github.com/swaywm/sway/blob/1.12/sway/sway-ipc.7.scd), [`sway/tree/view.c`](https://github.com/swaywm/sway/blob/1.12/sway/tree/view.c).

- **One call.** sway(5): "Criteria is retained across commands separated by a `,`" (reset by `;`). So `swaymsg '[con_id=N] floating enable, resize set W H, move absolute position X Y'` is valid. `floating enable|disable|toggle` applies to the criteria-matched window. `con_id` is "the internal container ID, which you can find via IPC".
- **Size.** `resize set [width] <w> [px|ppt] [height] <h> [px|ppt]`; for floating containers the default unit is px. A 0 leaves that axis unchanged.
- **Position.** `move [absolute] position <x> [px|ppt] <y> [px|ppt]` "moves the focused container to the specified position **in the workspace**"; "if `absolute` is used, the position is relative to **all outputs**" and `absolute` can't take ppt. So plain `move position` is workspace-relative, and `move absolute position` uses the global layout space (output origins included).
- **Reading geometry.** `get_tree` nodes carry `rect` ("the absolute geometry of the node", decorations excluded, borders included) and `window_rect` (relative to the node). `rect` is in px, in the same global space as `move absolute position`, so reference geometry from `get_tree` plus `absolute` lines up. Unconfirmed: behaviour with scaled outputs (rects are in logical px, as far as I know, but I didn't verify).
- **Tiled first?** If the window is already mapped and tiled, floating it later is a visible change: it leaves the tiling tree and the other tiles reflow. In `view_map` (view.c), a new view is first added as tiling (`workspace_add_tiling`), then `container_set_floating` only if the client itself `wants_floating`, then `arrange_*`, and only then `view_execute_criteria(view)`. So a `for_window` rule runs in the same handler, synchronously, right after the tiled insert and arrange. Whether a tiled frame is ever drawn between those steps is unconfirmed; a rule is at worst a single-frame flash, and I did not test it.
- **Rule by title.** `for_window [title="^NEOREX-FLOAT$"] floating enable, resize set …, move …` works: `view_update_title` runs before `view_execute_criteria`, so the title the client set before map is matched. Rules run once per criteria per view (`executed_criteria`), so a later title change doesn't re-trigger.

## 2. Hyprland (0.57.1)

Sources: wiki [Dispatchers](https://wiki.hypr.land/Configuring/Dispatchers/), [Window Rules](https://wiki.hypr.land/Configuring/Window-Rules/), [Using hyprctl](https://wiki.hypr.land/Configuring/Using-hyprctl/) (current = Lua; older pages at [wiki commit 84b5e06](https://github.com/hyprwm/hyprland-wiki/tree/84b5e067e3306ecafc88dd8ac2040cfd43aaa0ef/content/Configuring), 2026-03-28); source at [v0.57.1](https://github.com/hyprwm/Hyprland/tree/v0.57.1) (`src/config/shared/actions/ConfigActions.cpp`, `src/config/lua/bindings/LuaBindingsDispatchers.cpp`); [release notes](https://github.com/hyprwm/Hyprland/releases).

**Syntax history, so the command can pick by version:**

| Version | Change |
|---|---|
| 0.48.0 (2025-03-23) | windowrule v1 removed; `windowrule` now behaves like `windowrulev2`, which is deprecated |
| 0.53.0 (2025-12-29) | window/layer rule syntax "completely overhauled": `windowrule = float on, match:title REGEX` |
| 0.55.0 (2026-05-09) | Lua config becomes the default (`hl.window_rule{…}`, `hl.dsp.*`); legacy still supported |
| 0.57.0 (2026-10-28) | "config: remove legacy config support (#15539)" |

- **Classic dispatchers (≤ 0.56, from the old wiki page).** `setfloating` takes `window` (e.g. `address:0x…`); `resizewindowpixel` and `movewindowpixel` take `resizeparams,window` where `resizeparams` may be `exact W H`. Example batch: `hyprctl --batch "dispatch setfloating address:0xA; dispatch resizewindowpixel exact 800 600,address:0xA; dispatch movewindowpixel exact 100 50,address:0xA"`. `--batch` is documented ("separated with a `;`") and the docs recommend it over many calls.
- **0.57.1 (Lua).** Per the 0.57.1 `hyprctl(1)` page, `hyprctl dispatch` takes a Lua dispatcher string, e.g. `hyprctl dispatch "hl.dsp.window.close()"`. The wiki lists `float({ window?, action? })`, `move({ window?, x, y, relative? })` and `resize({ window?, x, y, relative? })`, where `window` is a "window selector" and `action = "enable"` floats. Unconfirmed: that `address:0x…` is a valid `window` selector string in 0.57's Lua dispatchers (the old selector list had it; I didn't open the new selectors page). Equivalent call: `hyprctl dispatch 'hl.dsp.window.float({ window = "address:0xA", action = "enable" })'`. How `--batch` handles Lua is documented only for semicolon escaping, so use one `eval`/`dispatch` per command, or put all three in a single Lua string via `hyprctl eval`.
- **Absolute position.** `Actions::move` computes `delta = pos − window->position(GEOMETRIC_GOAL)` and applies it, so non-relative `x, y` are in the same global layout coordinates as `at` from `hyprctl clients`. `Actions::resize` for absolute size rejects `< 1` and sets `delta = size − current size`. **Both require the window to be floating first**; resizing a tiled window changes tiling ratios. `float` goes through `changeFloatingMode`, so a window that is already tiled reflows.
- **Geometry.** `hyprctl -j clients` / `activewindow` give `at` and `size` (key names confirmed from the test suite, `hyprtester/src/tests/main/window.cpp`, which reads `at` from `/activewindow`). Monitor geometry is from `hyprctl -j monitors`. Static *rule* `move` values are monitor-local, while dispatcher `move` is global. Don't mix them.
- **Rule at map.** Static rules "are evaluated once when the window is opened and never again"; matching `title` therefore always uses `initialTitle`, and the wiki warns it is "not possible to `float` … a window based on a change in the `title` after the window has been created". A rule can set `float`, `move`, `size` together (so the window is never tiled), with expressions `monitor_w`, `monitor_h` etc. (monitor-local). Lua form: `hl.window_rule({ match = { title = "^X$" }, effect = { float = true, size = {…}, move = {…} } })`. Rules are config, not a command; a one-shot command can add one at runtime only via `keyword windowrule` (≤ 0.56, "will not work if your config provider is lua" per hyprctl(1)) or `eval`.

## 3. AeroSpace (0.21.3-Beta)

Sources: [commands docs](https://github.com/nikitabobko/AeroSpace/tree/main/docs) (`aerospace-layout.adoc`, `aerospace-resize.adoc`, `aerospace-move.adoc`, `aerospace-list-windows.adoc`, `aerospace-test.adoc`, `guide.adoc`), GitHub issues/PRs.

- **Float by id: yes.** `aerospace layout [--window-id <id>|--workspace <ws>] … (tiling|floating)`; "`tiling|floating` – Toggle between floating/tiling mode". With a single `floating` argument it is a no-op if already floating (`--fail-if-noop` makes that exit non-zero).
- **Absolute position/size: no.**
  - `aerospace resize [--window-id <id>] (smart|smart-opposite|width|height) [+|-]<number>` does take an absolute number, but the docs list no floating support, and [issue #9](https://github.com/nikitabobko/AeroSpace/issues/9) "resize command: support floating windows" is open (last updated 2025-06); [PR #1156](https://github.com/nikitabobko/AeroSpace/pull/1156) "Support resizing floating windows" is open.
  - `aerospace move [--window-id <id>] [--boundaries…] left|down|up|right` is directional only. [Issue #1417](https://github.com/nikitabobko/AeroSpace/issues/1417) (`--floating-pixels`) and [PR #2248](https://github.com/nikitabobko/AeroSpace/pull/2248) (2026-09, relative pixel moves) are open; neither gives absolute coordinates.
  - Related: [#1519](https://github.com/nikitabobko/AeroSpace/issues/1519) "Floating windows position and size is not tracked properly" (open).
  - So positioning/sizing a floated window would have to be done outside AeroSpace (e.g. macOS Accessibility API, or AppleScript via System Events). I did not research or test those; unconfirmed.
- **Geometry: none.** `list-windows --format` / `--json` variables (`util/interpolation-vars-window.adoc`): `window-id`, `window-id-native` (CGWindowID), `window-title`, `window-is-fullscreen`, `window-layout`, `window-parent-container-layout`. No x/y/width/height. Monitor vars: id, name, is-main, appkit index; no frame. A reference window's size would have to come from elsewhere (`CGWindowListCopyWindowInfo` via `window-id-native`, or the AX API; neither is confirmed here).
- **Float at detection.** `on-window-detected = [{ if = 'test %{window-title} ~= pattern', run = 'layout floating' }]`. `test` supports `=` and `~=` (case-insensitive regex substring) against interpolation variables, which include `window-title`. The guide warns: "Some windows initialize their title after the window appears … may not work as expected". Also `aerospace layout floating --for-next-detected-window` floats whatever AeroSpace detects next (applied before and after callbacks; "the scheduled layout wins over the callbacks"). That's an alternative to title matching if the command can launch the window itself, but not for an already-open one. The callbacks run once at detection; whether the window is drawn tiled first is unconfirmed (AeroSpace lays out via the AX API after detection).

## 4. Ghostty

Sources: [`src/config/Config.zig`](https://github.com/ghostty-org/ghostty/blob/main/src/config/Config.zig) (`title`), [`src/Surface.zig`](https://github.com/ghostty-org/ghostty/blob/main/src/Surface.zig), [`src/apprt/gtk/class/Overrides.zig`](https://github.com/ghostty-org/ghostty/blob/main/src/apprt/gtk/class/Overrides.zig), [`src/cli/new_window.zig`](https://github.com/ghostty-org/ghostty/blob/main/src/cli/new_window.zig). Read from `main` at 2026-11-07, not a tagged release.

- **OSC override: confirmed ignored.** Config docs: `title` "will force the title of the window to be this title at all times and Ghostty will ignore any set title escape sequences programs (such as Neovim) may send". Source: in `Surface.zig` the `.set_title` message handler returns early ("ignoring title change request since static title is set via config") when `config.title != null`. The static title survives Neovim's `set title`. (Neorex itself would see a fixed title, so it can't also use titles to show Neovim state in that window.)
- **Applied at creation.** `Surface.init` calls `performAction(.set_title, config.title)` as soon as the surface is initialised, before any shell output. So the title is set when the surface is created. Whether that is before the compositor's first map of the toplevel (so a `for_window`/Hyprland rule sees it from the start) is **unconfirmed**; it depends on GTK/AppKit window-realization order, which I did not trace.
- **Linux GTK `+new-window`.** `ghostty +new-window --title=X -e cmd` forwards the arguments to the running instance; `Overrides.parse` accepts `--command=`, `--shell-integration=`, `--working-directory=`, `--title=`, and the title becomes the surface's `title-override`. Needs `gtk-single-instance` / a running instance. `--class=` in `new_window.zig` selects the instance. Not checked: whether `title-override` also takes effect before first map or blocks OSC (it's a separate path from `config.title`).
- **macOS `open -na Ghostty --args --title=X`:** not verified. Ghostty's CLI accepts config keys as flags, so `--title=X` becomes `config.title` for that instance, which goes through the same `Surface.init` path. I didn't read the macOS Swift side.
- **Hyprland angle.** Because Hyprland matches the *initial* title and Ghostty sets it at surface init, a launch-time `--title` is the best bet there; the remaining gap is the unconfirmed ordering above.

## 5. tmux 3.8

Sources: [CHANGES (3.7c → 3.8)](https://github.com/tmux/tmux/blob/master/CHANGES), [tmux.1](https://github.com/tmux/tmux/blob/master/tmux.1) (master at fetch time; matches the 3.8 CHANGES entries).

- `new-pane` "creates a new floating pane". `-x`/`-y` set width/height in columns/lines (default half the window width, a quarter of the height); `-X`/`-Y` set the upper-left corner (default: cascaded from the top-left). All four accept a trailing `%` for a percentage of the window size. Other flags: `-B` border lines, `-T` title, `-M` follow a mouse drag, `-A` stay above a zoomed pane, `-O` modal, `-W` wait for the command to exit, `-E` / `-d` etc.
- **Existing panes.** `break-pane -W -s <src-pane>` "is lifted out of the tiled layout and made floating", with the same `-x -y -X -Y` options (percentages allowed); CHANGES: "break-pane can float a tiled pane, join-pane can tile a floating pane". `move-pane` and `resize-pane` (`-x`/`-y` absolute size) "can now move and resize floating panes".
