# Prior art for pane provider interfaces

Facts gathered while designing the pane provider interface (#81, under map #76):
what one-interface-over-many-multiplexers projects do, and what Neovim's own
multi-UI attach does. Checked against smart-splits.nvim 6a03b29 (master, v3
protocol 3.0.0), its v2 tree ac11b8e (`v2.1.1-final`, the last tree with
`lua/smart-splits/mux/`), its backend repos `smart-splits-nvim/backend-{tmux,
kitty,wezterm,ghostty,zellij-lua}` (HEAD at clone time: 9e9e6aa, 4ae1bcc,
ae840db, 6943639, 4e3e588), vim-tmux-navigator e41c431, and Neovim v0.12.0
(fc7e5cf). Paths are relative to each repo root; every `file:line` was re-checked
against those exact commits/tag. Neovim's `ui.txt` is
`runtime/doc/api-ui-events.txt` in 0.12; `--remote-ui` is in
`runtime/doc/remote.txt`.

## smart-splits.nvim v2: `lua/smart-splits/mux/` (ac11b8e)

Backends were in-tree: `tmux.lua`, `wezterm.lua`, `kitty.lua`, `zellij.lua`,
`herdr.lua`, plus `init.lua` (dispatch) and `utils.lua` (detection).

- The interface is the `SmartSplitsMultiplexer` class
  (`lua/smart-splits/types.lua:1-12`), 11 members:
  `current_pane_id() -> id|nil`, `current_pane_at_edge(dir) -> bool`,
  `is_in_session() -> bool`, `current_pane_is_zoomed() -> bool`,
  `next_pane(dir) -> bool`, `resize_pane(dir, amount) -> bool`,
  `split_pane(dir, size|nil) -> bool`, `type`, optional `on_init()` /
  `on_exit()`, and `update_mux_layout_details()`.
- All are required by the type but not all are real. Gaps are filled with stubs
  that return a safe constant, and `init.lua` then guesses:
  - zellij: `current_pane_id()` returns `-1` ("very expensive to ask zellij for
    the current pane id"), `current_pane_at_edge()` returns `false` (issue
    #477), `current_pane_is_zoomed()` returns `false`, `split_pane()` returns
    `false`, `resize_pane` ignores `amount`
    (`lua/smart-splits/mux/zellij.lua:14-61`).
  - kitty: `current_pane_at_edge()` returns `false`
    (`mux/kitty.lua:87`).
  - `update_mux_layout_details()` is an empty "not implemented" stub on
    tmux/wezterm/zellij (`tmux.lua:189`, `wezterm.lua:170`, `zellij.lua:88`); kitty's runs `kitty @ ls` async to
    refresh a cached layout (`kitty.lua:149`).
- Move success is inferred, not reported: `move_multiplexer_inner` reads
  `current_pane_id()`, calls `next_pane`, reads the id again, and calls it a
  move only if the id changed (`mux/init.lua:15-47`). zellij is special-cased
  by name ("assume it was successful", `init.lua:38-40`). This is what the v3
  rewrite removed (see below).
- The edge check is a second round trip: `current_pane_at_edge(direction)` is
  asked before every move unless `at_edge == wrap` (`init.lua:96`). tmux answers
  with `list-panes -f '#{&&:#{pane_active},#{pane_at_<edge>}}'`
  (`mux/tmux.lua:48-71`); wezterm with `wezterm cli get-pane-direction`, falling
  back to move-and-compare-and-move-back on older WezTerm
  (`mux/wezterm.lua:100-113`).
- What each backend runs, all as one-shot child processes except kitty/wezterm
  marker writes:
  - tmux: `tmux -S <socket> select-pane -L|R|U|D`, `resize-pane -L.. <n>`,
    `split-pane -h|-v [-b] [-l n]`, `display-message -p '#{pane_id}'`, `#F`
    for zoom (`mux/tmux.lua:84-143`).
  - wezterm: `wezterm cli activate-pane-direction`, `adjust-pane-size
    --amount n`, `split-pane --left|--right|--top|--bottom [--cells n]`,
    `list --format json`, `list-clients` (`mux/wezterm.lua:77-157`).
  - zellij: `zellij action move-focus|move-focus-or-tab <dir>`, `zellij action
    resize increase <dir>` (`mux/zellij.lua:30-57`).
  - kitty: `kitty @ kitten neighboring_window.py|relative_resize.py|
    split_window.py ...`, i.e. user-installed Python kittens run inside kitty
    (`mux/kitty.lua:109-146`, `kitty/*.py`); `neighboring_window.py` forwards
    the key to the child instead when the foreground command is `tmux`
    (`kitty/neighboring_window.py:26-35`, `handle_result`).
  - herdr: `herdr pane list|edges --current|focus --direction ...` with JSON
    results (`mux/herdr.lua:44-146`).
- Detection was by environment, in `lua/smart-splits/config.lua:99-110` and
  `mux/utils.lua:3-42`: `HERDR_ENV`, `TERM_PROGRAM == tmux`, `ZELLIJ`,
  `TERM_PROGRAM == wezterm`, `KITTY_LISTEN_ON`; all skipped when Neovim is a GUI
  (`are_we_gui`, `mux/utils.lua:53-59`: its own UI has neither stdin nor stdout a tty,
  checked on `chan == 1` in `nvim_list_uis()`). The user could override with
  `vim.g.smart_splits_multiplexer_integration` (`config.lua:80-88`).
- Defaults depended on the mux: `at_edge` defaults to `stop` for kitty and
  zellij and `wrap` otherwise (`config.lua:44`), because those two cannot answer
  "am I at the edge".
- The other direction is a marker the mux reads: Neovim announces "I own this
  pane" and the mux's key binding branches on it.
  - tmux: pane option `@pane-is-vim` via `tmux set-option -pt $TMUX_PANE`
    (`mux/tmux.lua:149-187`), with nested-Neovim handling.
  - kitty and wezterm: OSC 1337 `SetUserVar=IS_NVIM=<base64>` written to the
    terminal (`mux/kitty.lua:126-136`, `mux/wezterm.lua:158-168`).
  - Set on startup and `VimResume`, cleared on `VimSuspend` / `VimLeavePre`
    (`mux/utils.lua:74-107`, `M.startup`).

## smart-splits.nvim v3: backends out of tree (6a03b29)

The v3 refactor (`69aa22d`, "refactor(v3)") moved every backend to its own
plugin and cut the interface down.

- Interface (`PROTOCOL.md:31-47`, `lua/smart-splits/backend.lua:73-84`):
  required `name`, `protocol_version`, `detect() -> bool`,
  `move(direction, opts) -> bool`; optional `resize`, `activate()`, `health()`.
  Split is not an operation of its own: `at_edge = 'split'` is passed to `move`
  and the backend splits (`PROTOCOL.md:103-163`, "`move(direction, opts)`"; backend-tmux
  `move.lua:41-42`, `at_edge == 'split'` branch).
- "A backend only answers one question, in three variations: did you handle
  this?" (`PROTOCOL.md:7`). The boolean is the whole result.
- Capability gaps: "There is no capabilities table. If your multiplexer cannot
  resize, leave the function out entirely. Core checks whether the function
  exists." (`PROTOCOL.md:83-84`; `backend.lua:281-290`, `call` returns false
  when `backend[op]` is nil). A missing or failing op degrades to Neovim-only
  behaviour; a forgotten return is treated as "not handled"
  (`backend.lua:305`).
- Dropped on purpose (`PROTOCOL.md:436-443`, "What core does not give you"):
  pane identifiers, layout details, edge queries ("Core cannot see your layout
  and does not try"). Each backend now does its own edge logic: backend-tmux's
  `move` asks `at_edge` first because "`select-pane -L` wraps around the window
  and reports success either way" (`backend-tmux` `move.lua:27-29`).
- Detection is explicit, not automatic: "There is no auto-detection. Naming your
  backend explicitly is what replaced it." (`README.md:413`;
  `PROTOCOL.md:441`). The user lists backends in priority order and the first
  whose `detect()` is true wins, resolved once at `setup()` and cached
  (`README.md:434-461`, `backend.lua:207`, `M.resolve`).
- `detect()` must be cheap: env vars, `executable`, at most a `fs_stat`; "No
  subprocesses" (`PROTOCOL.md:94`). Examples: tmux `is_in_session()` (the
  `$TMUX` socket), zellij `is_running() and exists()`, ghostty
  `TERM_PROGRAM == 'ghostty'` and `osascript` present and not under
  SSH/tmux/zellij (`backend-ghostty` `ghostty.lua:18-25`, `M.detect`).
- Per-key performance contract: each op runs on every keypress that reaches a
  window edge; core warns past 100 ms (`PROTOCOL.md:226-233`,
  `config.diagnostic.slow_threshold`), and the example passes a `timeout` of
  200 ms to `vim.system` (`PROTOCOL.md:231`).
- Errors never propagate: every call is `pcall`ed and the error is logged once
  per (backend, op, message) (`backend.lua:281-310`).
- Lifecycle is one optional hook, `activate()`, called only on the winning
  backend; `setup()` is deliberately outside the protocol and must be inert
  (`PROTOCOL.md:180-209, 238-280`).
- Conformance is shipped as data: `lua/smart-splits/protocol_tests.lua` returns
  `{name, fn}` cases a backend runs in its own suite (`PROTOCOL.md:357-402`).
  The `protocol_version` is a semver range; core accepts any overlap with
  `^3.0.0` and "additions that do not break existing backends do not bump the
  major version" (`PROTOCOL.md:9-27`).
- Options may grow without a version bump: backends "ignore fields you do not
  recognise" and "every field is optional" (`PROTOCOL.md:64-69`).

### How the v3 backends actually reach the multiplexer

- tmux: one-shot `tmux` processes (`select-pane`, `resize-pane`,
  `split-window -c '#{pane_current_path}'`, `list-panes -f
  '#{&&:#{pane_active},#{pane_at_<edge>}}'`) in `backend-tmux`
  `lua/smart-splits-backend-tmux/tmux.lua:170-226`. The `@pane-is-vim` contract
  is documented in `activate.lua:1-12`: "Unset reads as false, so a pane whose
  marker is missing has `C-hjkl` swallowed by tmux and Neovim never sees the
  key."
- kitty: not the CLI. It writes kitty's remote-control JSON directly to the
  `KITTY_LISTEN_ON` socket (`backend-kitty` `README.md:7,121-136`,
  `kitty.lua:23-40`), including the password and `kitty_window_id` fields in
  each request; detection is "is the socket usable"
  (`kitty.lua:53`, `usable()`). Requires `allow_remote_control` in kitty.conf
  (`README.md:158`).
- wezterm: deliberately not the CLI either. "Neither half of this integration
  shells out to `wezterm cli`... round-tripping through a CLI subprocess ...
  would just be slower and less capable (WezTerm's Lua API exposes pane
  geometry that its CLI never has)" (`backend-wezterm` `README.md:129-133`).
  Instead the plugin writes OSC 1337 `SetUserVar` escapes to the terminal
  (`SMART_SPLITS_REQ` carries `{action, dir, amount}`; `IS_NVIM` carries state)
  and a WezTerm-side Lua plugin acts on them (`README.md:135-141`,
  `userscript.lua:1-30`). `detect()` also requires the GUI plugin to have
  announced itself via `$SMART_SPLITS_WEZTERM`, because with nothing listening a
  user-var write "always succeeds and nothing happens" (`init.lua:15-21,30`).
  Needs `wezterm.lua` config in the terminal (`README.md:51-60`).
- ghostty (macOS): AppleScript through `osascript -l JavaScript
  scripts/ghostty.js <action> ...`; the backend has two transports: a persistent
  `osascript ... serve` process speaking newline-delimited JSON, and a one-shot
  `osascript` fallback per request (`transport.lua:1-6`, `ghostty.lua:1-8,
  37-55`). The script finds its own terminal by walking the process tree from
  its pid to the owning app (`ghostty.js:62-100`) and asks for the focused
  terminal id (`ghostty.js:181-196`) to pass as the target of each action.
  Ghostty needs a user binding table in its config, and Neovim "claims" and
  "releases" keys on enter/leave (`lifecycle.lua:29-45`; `README.md` "How it
  works", "Requirements": Ghostty 1.3+, macOS Automation permission).
- zellij: `backend-zellij-lua` shells out to `zellij action` (`move.lua:216` `move_or_wrap`, `zellij.lua:52-129`); a Rust rewrite (`backend-zellij-rs`) is "in
  progress" per `README.md:419`. Why a rewrite is wanted is unconfirmed here.
- A third-party Rex backend exists, `rawnly/smart-splits-backend-rex`, listed in
  `README.md:432`. Its contents were not read.

## vim-tmux-navigator (e41c431)

One pair of halves, no interface: a Vim plugin and a block of tmux config.

- Vim side (`plugin/tmux_navigator.vim`): `s:TmuxAwareNavigate` runs `wincmd`,
  compares `winnr()` before and after, and only if unchanged forwards
  (`:121-158`). Forward = `tmux -S <socket> select-pane -t $TMUX_PANE -L|D|U|R`
  (or `-l` for previous) (`:143`). That is the entire operation set: focus in
  four directions plus "last". Detection: `empty($TMUX)` decides whether to
  define the tmux-aware commands at all (`:43-50`); the socket is the first
  comma field of `$TMUX` (`:84-87`); `tmate` is detected from the same variable
  (`:76-78`).
- Optional behaviours are plain variables that wrap the one command:
  `g:tmux_navigator_disable_when_zoomed` (`display-message -p
  '#{window_zoomed_flag}'`, `:80-82,114`), `preserve_zoom` (`-Z`, `:144-146`),
  `no_wrap` (`if -F "#{pane_at_<pos>}"`, `:147-149`), `save_on_switch`
  (`:132-142`). Missing tmux features are not an issue since tmux is the only
  target.
- tmux side (`README.md:97-113, 327-337`): each key is bound as `if-shell
  "$is_vim" 'send-keys C-h' 'select-pane -L'`. `is_vim` is a shell snippet:
  `ps -o state= -o comm= -t '#{pane_tty}' | grep -iqE '^[^TXZ ]+
  +(\S+/)?g?\.?(view|l?n?vim?x?|fzf)(diff)?(-wrapped)?$'`, i.e. identify the
  pane's program by its tty's process list (`README.md:97-98,151-159`). The
  pattern is user-overridable (`@vim_navigator_pattern`, `README.md:151`) and
  the whole check is overridable (`@vim_navigator_check`, `README.md:159`).
  The `ps` check is a pull model; smart-splits' marker (above) is a push
  model.
- The "no wrap" variant has the multiplexer test `#{pane_at_left}` etc.
  itself (`README.md:329-337`).

## Neovim 0.12 multi-UI attach

### How UIs attach

- `nvim --remote-ui --server ADDR` runs the built-in TUI as a client of an
  existing server, "fully interactive: keyboard and mouse input are forwarded"
  (`runtime/doc/remote.txt:55-58`). Any program can instead call
  `nvim_ui_attach(width, height, options)` on a running Nvim; "UIs can also
  connect to a running Nvim instance" (`api-ui-events.txt:25-28`).
- `nvim_ui_attach` is RPC-only, errors if the channel already has a UI
  ("UI already attached to channel") or if 16 are attached ("Maximum UI count
  reached"; `MAX_UI_COUNT 16`, `src/nvim/ui.c:53`; `src/nvim/api/ui.c:157-168`).
  `nvim_list_uis()` lists attached UIs (`api.txt:1217`).
- `nvim_ui_detach()` removes the UI (`api.txt:3717`); `nvim_ui_set_focus`
  tells Nvim a UI gained or lost focus (`api.txt:3774`).
- Every UI is sent every redraw batch: "Nvim sends RPC notifications to all
  attached UIs" (`api-ui-events.txt:71-72`); `UI_CALL` loops over all UIs
  (`ui.c:104-118`).

### How the screen size is agreed

- Rule: "If multiple UI clients are attached, the global screen dimensions
  degrade to the smallest client. E.g. if client A requests 80x40 but client B
  requests 200x100, the global screen has size 80x40." (`api.txt:3704-3707`).
- Implementation: `ui_refresh()` takes `MIN(ui->width, width)` and
  `MIN(ui->height, height)` over all UIs and calls `screen_resize()`
  (`src/nvim/ui.c:188-246`, specifically `:202-203` and `:245`). Width and
  height are agreed independently, so the result can come from two different
  UIs.
- `nvim_ui_try_resize` only changes that UI's own requested size and calls
  `ui_refresh()` (`src/nvim/api/ui.c:307-324`). Attach and detach also call
  `ui_refresh()` (`ui.c:410, 457`), so the screen re-agrees whenever a UI
  comes or goes.
- Consequence: a larger UI shows Nvim's screen at the smaller size; there is no
  per-UI viewport. I found no mechanism in `ui.c` that gives a UI its own
  size (checked by reading; not tested).

### How capabilities are agreed

- "By default Nvim requires all connected UIs to support the same capabilities,
  thus the active capabilities are the intersection of those requested"
  (`api-ui-events.txt:66-68`). `override = true` "inverts this behavior" and
  enables requested extensions "even if not supported by all connected UIs
  (including TUI)" (`:35-41`).
- Implementation: `ext_widgets[j] &= (ui->ui_ext[j] || inclusive)` over all
  UIs, where `inclusive` is true if any UI set `override` (`ui.c:171-180,
  197-206`). The result is announced with `option_set` events (`ui.c:213-217`;
  `api-ui-events.txt:69`).
- `ext_multigrid` is one of those extensions (`kUIMultigrid`,
  `src/nvim/ui_defs.h:19`), and the gate the rest of the code uses is the
  intersected global `ui_ext` via `ui_has()` (`ui.c:704-707`; e.g.
  `ui_compositor.c:322`, `winfloat.c:241`). So with a plain UI such as the TUI
  attached, multigrid is documented to be off unless some UI sets `override`.
- What a multigrid client is *told* is subtler. `ui_refresh()` broadcasts the
  effective value with `option_set` only for the extensions below
  `kUIGlobalCount` (cmdline, popupmenu, tabline, wildmenu, messages;
  `ui.c:213-217`, `ui_defs.h:12-17`). For the others (linegrid, multigrid,
  hlstate, termcolors), `ui_set_ext_option` sends `option_set` only to the
  attaching UI, with that UI's own requested value rather than the
  intersection (`ui.c:400, 454-462`). So a multigrid client next to a TUI may
  be told `ext_multigrid` is true while grid events stay off. This is read from
  the code only; unconfirmed by running it.
- Multigrid UIs are not attached to the compositor: `ui_attach_impl` calls
  `ui_comp_attach` only when the UI has neither `ext_multigrid` nor
  `ext_float_debug` (`ui.c:384-387`); `ui_compositor.c:322` checks
  `ui_has(kUIMultigrid)` globally.
- With multigrid active, "UIs can set the grid size independently of how much
  space the window occupies on the global layout"; once a UI sets a grid size,
  "Nvim does not handle the size for that grid", and requesting (0, 0)
  "delegate[s] grid-size handling back to Nvim"
  (`api-ui-events.txt:615-626`; `nvim_ui_try_resize_grid`, `api.txt:3804-3815`;
  `api/ui.c:453-466`). Grid sizes are shared global state, not per UI:
  `nvim_ui_try_resize_grid` on a real grid calls `ui_grid_resize` once. I did
  not find a per-UI grid size.
- `rgb`, `term_name`, `stdin_tty`, `stdout_tty` are per-UI options
  (`api-ui-events.txt:31-33, 56-61`); `ui_rgb_attached` and `ui_gui_attached`
  scan all UIs for any that is non-TUI (`ui.c:142-170`). smart-splits v2 uses
  the `chan == 1` UI's tty flags to decide whether it is in a terminal at all.
- Startup: an embedder runs `nvim --embed` (no `--headless`) and Nvim "will
  pause before loading startup files" until `nvim_ui_attach`
  (`api-ui-events.txt:122-129`).

## What this means for the pane provider interface

- Command templates are viable as the minimum, and v2 smart-splits shows the
  floor. Of an 11-member interface, every working operation across tmux,
  wezterm, zellij and herdr was a one-shot CLI call: focus-direction, resize,
  split, plus read-only queries (edge, pane id, zoom). The exceptions are
  kitty (which needs user-installed kittens, then a socket protocol in v3)
  and wezterm (v3 chose escape sequences over its CLI). So "no Rust, no
  long-running process" fits tmux, zellij, herdr, kitty `@` (with
  `allow_remote_control`), and ghostty/AppleScript (one-shot `osascript`;
  ghostty's backend itself keeps a one-shot fallback). WezTerm is the weak
  point: its CLI exists but its own backend author calls it "slower and less
  capable".
- The minimal operation set that survived in v3 is tiny: "did you handle
  this?" for move, optional resize, a cheap `detect`. smart-splits had to remove
  `current_pane_id` and `current_pane_at_edge` because several muxes could not
  answer them cheaply or at all (zellij, kitty), and because inferring success
  by comparing ids was a workaround. For neorex the required commands should be
  about what neorex must do to get panes (create a pane running a given command,
  close a pane, report the pane's identity or that it exists); focus/place/
  arrange stay optional, as the map already prefers.
- Capability gaps: three precedents.
  1. v2: stub that returns a safe constant plus special cases in the caller
     (`zellij` by name in `mux/init.lua:30`). This is the failure mode to avoid.
  2. v3: omit the function; core checks presence; no capabilities table
     (`PROTOCOL.md:83`). Simple, and it works because every op has a Neovim-only
     fallback.
  3. wezterm's backend: "detect()" is extended to check the whole integration
     is wired, to avoid a silently-succeeding write (`init.lua:15-21`).
  For commands: an unset template meaning "unsupported" is the same idea as
  an omitted function. Pair it with a way to check it works (a `detect`
  template, or a health check) so a missing config does not silently no-op.
- A bare "handled: bool" result is enough only when there is a fallback.
  smart-splits can fall back to Neovim's own windows, so a lost move is
  harmless. neorex's required operations (create a pane) have no fallback, so
  they need an exit status, and probably stdout (a pane id), plus stderr for
  the user.
- Detection: v3 abandoned auto-detection and made the user name the provider,
  with a cheap, subprocess-free `detect` used for priority lists. v2's detect
  was environment variables. A neorex provider config should say how it is
  chosen, and a `detect` that is cheap (env var) is a precedent for allowing a
  command template but warning that it runs per attach.
- Per-call cost matters for command templates. smart-splits sets a 100 ms
  warning and 200 ms example timeout because ops run per keypress. neorex pane
  creation is rarer but focus on events is not; a one-shot per focus change
  may be acceptable, `osascript` startup is the cost to measure (not measured
  here).
- Identity of "my own pane" is the recurring hard part and is solved per
  provider by environment: `$TMUX_PANE` (tmux, vim-tmux-navigator `:143`),
  `$KITTY_WINDOW_ID` (kitty `kitty.lua:13-14`), `$ZELLIJ`, `HERDR_ENV`,
  process-tree walk to the owning app (ghostty `ghostty.js:62-100`). For
  neorex, the attach handshake can carry the provider's own id back to the
  daemon (env var inherited by the `neorex pane` process, or a command that
  prints it), so templates can use `{pane}` style substitution without the
  daemon knowing each provider's env names.
- The reverse channel (the program in the pane telling the mux "I own this
  pane") is a separate need from commands: tmux option, OSC 1337 user var
  (kitty, wezterm), key tables (ghostty). It matters for key routing, not for
  getting panes; for neorex it is probably out of scope for the required set,
  but terminal-written escape sequences work without Rust or a daemon and
  never need a tty fd to be passed, which suits "nothing may make Windows
  hard".
- Multi-UI Neovim argues against neorex becoming "several UIs" per Neovim for
  its panes. All UIs share one screen sized to the minimum of their requests
  (`api.txt:3704`; `ui.c:202-203`), one capability set that is the
  intersection (`ui.c:205`; `api-ui-events.txt:66`), a cap of 16 UIs
  (`ui.c:53`), and a mixed plain-plus-multigrid pair disables multigrid for all
  unless one sets `override`. A neorex that keeps the single multigrid UI
  (the daemon) and has panes attach to it, as the map says, avoids all of
  that; attaching panes as extra Neovim UIs would not work. The mismatched
  sizes also say pane sizes should be the daemon's concern, reported by the
  provider as optional live layout, not something Neovim negotiates.
- Provider config versioning: smart-splits' `protocol_version` as a semver
  range, additions without a major bump, "ignore unknown fields", and a
  conformance test suite shipped as data (`PROTOCOL.md:9-27,64-69,357-402`) are
  precedents for neorex's provider interface to grow optional capabilities
  without breaking user-written providers.

## Not confirmed

- Whether zellij's or kitty's CLI can answer "pane at edge" or "pane id" cheaply
  today. v2 stubs them; `backend-zellij-lua` 4e3e588 now calls `zellij action
  list-panes --json`, `current-tab-info --json`, `focus-pane-id` and `new-pane
  --direction` (`zellij.lua:66-212`), so zellij's CLI has gained queries, but
  their cost and zellij's own docs were not read.
- `backend-zellij-rs` and `smart-splits-backend-rex` contents; latency of
  `osascript` and of `tmux`/`wezterm cli` invocations per call.
- Runtime behaviour of an `ext_multigrid` UI next to a TUI, including which
  `option_set` events it receives (derived from `ui.c` and docs only, not run).
