# Tab-page places: focusing and opening across tmux, herdr, Neovim, AeroSpace

Research for placing a neorex Tab page in a host multiplexer, window or tab.
Short answer first in each section, then evidence. "Verified" means I ran it
here; "read" means taken from source or docs only.

Pins (all source cited as `file:line` at these):

| Project | Pin |
| --- | --- |
| tmux 3.8 | tag `3.8`, commit `7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53` (`https://github.com/tmux/tmux/tree/3.8`) |
| tmux 3.5a | tag `3.5a`, commit `549c35b06165f6ae023115eb76f83f2cbf945395` |
| herdr 0.9.5 | tag `v0.9.5`, commit `2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01` (`https://github.com/herdrdev/herdr/tree/v0.9.5`) |
| Neovim 0.12.6 | tag `v0.12.6`, commit `779c36969e31080503be2dfccbcc3f999e289606` (`https://github.com/neovim/neovim/tree/v0.12.6`) |
| AeroSpace | `main` at `0921fb810de260b89f7c53abc00b249e47e1fc1d` |

Local environment: tmux 3.8 (installed with brew for this research), herdr
0.9.5 (a throwaway headless `herdr server` on a private socket,
`HERDR_SOCKET_PATH=/tmp/...`), nvim 0.12.6 `--clean --headless`. AeroSpace
CLI 0.21.3 is installed but AeroSpace.app is not running, so nothing about it
was run. The tmux server (`-L research90`), the herdr server and nvim were all
stopped afterwards.

## 1. tmux

### Short answers

- `select-pane -t %N` does **not** switch the client's window. It only makes
  `%N` the active pane of its own window. Verified, 3.8; read, 3.5a and 3.8.
- `switch-client -t %N` **does** switch: session, window and pane. A target
  containing `:`, `.` or `%` is parsed as a pane target. Verified, 3.8.
- One-shot "make this pane current for the user, wherever it is":
  `tmux switch-client [-c <client>] -t %N`. Same session or another session.
  Outside a client context (a script with no attached tty) pass `-c`.
  `select-window -t %N` also works within the current session, but it can't
  cross sessions.
- `new-window -a -t %N ...` **fails**: `can't specify pane here`.
  `new-window`'s `-t` is a window target that forbids a pane part, and a `%N`
  pane id is one. Use the window id instead:
  `new-window -a -t "$(tmux display -p -t %N '#{window_id}')" -P -F '#{pane_id}' cmd arg...`.
  With that, the new window opens right after the target's window, the command
  prints the new pane id, and the new window **does** become the client's
  current window unless you pass `-d`. Verified, 3.8.

### Evidence

`select-pane`:

- 3.8 `cmd-select-pane.c:274-290`: after `if (wp == w->active) return`, it only
  calls `window_set_active_pane(w, wp, 1)`, then redraws. There is no
  `session_set_current`. (The 3.5a code is the same: `cmd-select-pane.c:227-`
  with `window_set_active_pane`, no `session_set_current`, in
  `cmd_select_pane_exec`.)
- Verified: client on `a:0` (window 0), `select-pane -t %2` where `%2` was the
  inactive pane of `a:1` left `list-clients` at `win=0 pane=%0`, while
  `list-panes -t a:w1` showed `%2 active=1`. `select-pane -t %3` (pane in
  session `b`) also left the client on `a:0`.

`switch-client`:

- 3.8 `cmd-switch-client.c:68-76`: a `-t` containing any of `:.%` (or equal to
  `=`) is resolved as `CMD_FIND_PANE`, else as a session.
- 3.8 `cmd-switch-client.c:138-156`: with a client, it makes the pane the
  active pane of its window if it isn't (`window_set_active_pane`, zooming
  state preserved unless `-Z`), then `session_set_current(s, wl)` to select
  the window, then `server_client_set_session(tc, s)` moves the client to
  session `s`. 3.5a does the same, `cmd-switch-client.c:119-129`.
- Verified: `switch-client -c <tty> -t %2` moved the client from `a:0` to
  `a:1` with `pane=%2`. `switch-client -t %3` moved it to session `b`. Both
  were "current" per `list-clients -F '#{session_name} #{window_index} #{pane_id}'`.
- Verified: `select-window -t %2` also switched within the session
  (`a:0` -> `a:1`, `pane=%2`).
- man page: <https://man.openbsd.org/tmux> (`switch-client`: "Switch the
  current session for client target-client to target-session"; the pane
  target form is the source above).

`new-window`:

- 3.8 `cmd-new-window.c:44-47`: `.target = { 't', CMD_FIND_WINDOW,
  CMD_FIND_WINDOW_INDEX }`. `cmd-find.c:1157-1159`: with
  `CMD_FIND_WINDOW_INDEX`, a pane in the target yields `can't specify pane
  here`. Verified, `new-window -a -t %2 -P -F '#{pane_id}' sleep 998` printed
  `can't specify pane here` and created nothing. This is true for `-d` and for
  a pane in another session too. 3.5a has the same target flags
  (`cmd-new-window.c:45`).
- `-a`: `cmd-new-window.c:130-133` does `winlink_shuffle_up(s, wl, before)`, so
  the new window takes the index right after the target window and later ones
  shift up. Verified with window id `@1` (the window of `%2`): windows went
  from `0,1` to `0,1,2` with the new one at `2`. With a window in the middle
  (`-a -t @1` with `-d` while `a:2` existed) the new window took index `2` and
  the old `2` moved to `3`.
- `-P -F '#{pane_id}'`: `cmd-new-window.c:171-179` formats the new winlink's
  active pane. Verified: it printed `%4`, and with `-d` `%5`.
- Current window: `spawn.c:229-230` calls `session_select` unless
  `SPAWN_DETACHED`; `cmd-new-window.c:155-157` sets that flag for `-d`. Verified:
  without `-d` the client moved to `a:2`. With `-d` it stayed on `a:0`.
- Target in another session works by window id: `-d -a -t <window id of %3>`
  created `b:1` and printed its pane id; the client stayed on session `a`.
- No shell: `spawn.c:547-552` uses `execvp` when given multiple arguments.
  With a single argument, `spawn.c:561` runs it via the shell. Verified with
  `ps`: `sleep 997` had parent pid = the tmux server and no `sh -c` in between,
  when given as separate arguments.

## 2. herdr

### Short answers

- `pane.focus` on a pane in another tab **does** switch to that tab. It also
  switches the active workspace if the pane is in another workspace. Verified
  (tabs, headless server) and read.
- `layout.apply` **does** create a new tab running an argv with no shell, and
  its response **does** carry the new pane's id. Verified.
  - Request: `{"method":"layout.apply","params":{"root":{"type":"pane","command":["prog","arg"], "cwd":"..."}}}`.
    A `command` leaf is a flat argv on the pane node. Not nested under a
    `pane` key: a nested one is silently ignored and gives a plain shell.
  - Response: `result.layout.tab_id`, `result.layout.focused_pane_id`, and
    `result.layout.root.pane_id` (e.g. `w1:t6`, `w1:p6`).
  - It does not steal focus unless `"focus": true` is passed.
- There is no CLI subcommand for `layout.apply`. It is socket-only, so a
  client must speak the newline-delimited JSON protocol itself (or use
  `herdr api`'s socket path). The prior note's "unconfirmed" items on argv
  no-shell and live behaviour are now confirmed below.
- The pane is removed when the command exits, and the tab goes with it if
  it was the only pane. Verified.

### Evidence

- `pane.focus`: `src/app/api/panes.rs:491-507` (`handle_pane_focus`) calls
  `focus_pane_in_workspace`. That is `src/app/actions.rs:226-251`: it calls
  `switch_workspace_tab(ws_idx, tab_idx)` then `tab.layout.focus_pane`.
  `switch_workspace_tab` (`src/app/actions.rs:406-`) sets `self.active =
  Some(ws_idx)` and switches the tab. Caveat: it returns early with `false`
  (but still `mark_active_tab_seen`, and still a success response) when the
  pane is already the focused one.
  Verified on a headless server with tabs `t1,t2,t3`: `pane.focus w1:p1` ->
  `t1` focused; `pane.focus w1:p6` (in `t6`) -> `t6` focused, others not.
  Caveat on verification: headless means no TUI client attached, so "focused"
  is server state, which is what an attached client renders. Cross-workspace
  focus was read only, not run.
- Raw only: the CLI has no `pane focus <id>` (prior note,
  `docs/research/herdr-pane-provider.md` on `origin/research/herdr-pane-provider`).
  Raw call: `{"id":"1","method":"pane.focus","params":{"pane_id":"w1:p6"}}` to
  the socket.
- `layout.apply`: `src/app/api/layouts.rs:34-` is `handle_layout_apply`.
  - With no `tab_id`/`workspace_id` it targets the active workspace (`:63-65`)
    and creates a new tab (replace only if `tab_id` is given, `:52`).
  - `layout_command` (`:577-583`) turns the root leaf's `command` into argv. An
    empty array is an error.
  - A command leaf calls `ws.create_tab_argv_command` (`:114-125`), which calls
    `src/workspace.rs:486-508` and eventually `Pane::spawn_argv_command`
    (`src/pane.rs:2456-2495`): `CommandBuilder::new(program)` plus `.arg()` per
    element, no shell. Verified: `ps` showed `sleep 991` as a direct child of
    the herdr server.
  - Focus only moves with `params.focus` or when replacing the active tab
    (`:197-200`). Schema: `LayoutApplyParams` is `src/api/schema/panes.rs:148-158`
    (`workspace_id`, `tab_id`, `tab_label`, `focus` defaulting to false, `root`);
    `LayoutPane` is `:195-206` (`pane_id`, `label`, `cwd`, `command`, `env`).
  - The response is `ResponseResult::LayoutApply { layout }`, from
    `layout_description` (`:310-322`): `workspace_id`, `tab_id`, `zoomed`,
    `focused_pane_id`, and the tree with `pane_id` on each leaf.
    Verified, output (abridged): `{"layout":{"workspace_id":"w1","tab_id":"w1:t6","focused_pane_id":"w1:p6","root":{"type":"pane","pane_id":"w1:p6","cwd":"/private/tmp","command":["sleep","991"]}}}`.
  - Verified: without `focus` the focused tab stayed `t2`; with
    `"focus": true` the new tab became focused.
- Exit: `["true"]` as the command created `w1:t4`, and a second later it was
  gone from `tab list`. `herdr pane close w1:p6` killed the `sleep` (it was not
  in `ps` a second later). Verified.
- Socket-only: `docs/next/website/src/content/docs/socket-api.mdx:108` lists
  `layout.apply` under Layout methods; `:212` describes it. No `layout`
  subcommand exists in `src/cli/` (grep).
- Closest CLI-only alternative, if the socket is unwanted: `herdr tab create
  [--focus]` returns `.result.root_pane.pane_id`, but runs a shell and takes no
  command (prior note); follow with `herdr pane run <id> "<cmd>"`, which types
  into the shell (the shell stays when the command exits).

## 3. Neovim 0.12

### Short answers

- `CTRL-W T` **creates a new window** (a new window id) in a new tab page,
  initialised from the old one, then closes the original. The id is not kept.
  Verified.
- `:tab split` makes a new window in a new tab; the original stays. Verified.
- There **is** an API to move a window between tab pages keeping its id:
  `nvim_win_set_config(win, {split=..., win=<window in the other tab>})`, and for
  a float `{relative=..., win=<window in other tab>}`. Verified for a split and
  for a float. It cannot move the last non-floating window of a tab, nor
  external windows (see below). There is no way to move to a brand-new tab
  with it; you need an existing window in the target tab.
- `win_splitmove` (`:wincmd J/K/H/L`) is current-tab only, so it is not that API.
- `CTRL-W T` with a float or external window as the current window is **not**
  refused by `one_window()`: `one_window()` ignores floats. Verified for a
  normal float (it works, the float is replaced by a normal window in a new
  tab). External windows: **not verified**, because headless has no
  multigrid UI (`nvim_open_win` with `external=true` returns "UI doesn't
  support external windows"). By reading, it is also not refused.
- Floats are tied to a tab page and are hidden when that tab isn't current.
  External windows are not: they follow the current tab page.

### Evidence

- `CTRL-W T`: `src/nvim/window.c:487-509`. It checks `one_window(curwin, NULL)`
  and if true shows `Already only one window` (`window.c:104`, with no error
  number; `E1159` is `Cannot split a window when closing the buffer`, `:102`,
  and the float variant `api/win_config.c:287`, not a one-window refusal).
  Otherwise it calls `win_new_tabpage(Prenum, NULL, true, NULL)`, then goes
  back to the old tab and `win_close(curwin, ...)` if `curwin == wp` (`:497-502`),
  then returns to the new tab and fires `TabNewEntered`.
- `win_new_tabpage` (`window.c:4490-`): `win_alloc_firstwin(old_curtab->tp_curwin)`
  (`:4525`) allocates a new window (`win_alloc`, `:4382`) and `win_init`s it from
  the old one (`:4388-4392`). So the new window has a new handle. The docs say
  the same: `runtime/doc/windows.txt:547-550` ("works like `:tab split`, except
  the previous window is closed").
- Verified (ids from one run): `vsplit`, then `wincmd T`: old `1001` -> new
  `1002`, `nvim_win_is_valid(1001) == false`, 2 tab pages. `tab split`: `1002`
  -> `1003`, old still valid.
- Move keeping id: `nvim_win_set_config` doc (`src/nvim/api/win_config.c:739-747`):
  "Can move a window to another tabpage", "Cannot move the last window in a
  tabpage to a different one". `win` "Can be in a different tab page"
  (`:186-188`). The split path is `win_config_split` (`:434-`), and the float path
  `win_config_float_tp` (`:647-`). The checks are in `win_can_move_tp`
  (`:389-421`): refuses the last non-floating window (`one_window`), a window
  locked/in use, autocmd window, cmdwin, textlock. `runtime/doc/news.txt:119`
  mentions "can move floating-windows to other tabpages".
  Verified: window `1006` split into another tab with `{split="right", win=w2}`
  stayed valid and `nvim_win_get_tabpage` became the other tab, current tab
  unchanged. A float moved with `{relative="win", win=<win in other tab>, row=0,
  col=0}` kept its id and moved tabs. Moving the only window of a tab gave
  `Cannot move last non-floating window`.
- `win_splitmove`: `window.c:2058-2112`, comment "wp must be valid in the
  current tabpage".
- `one_window`: `window.c:2660-2666`: `first == win && (win->w_next == NULL ||
  win->w_next->w_floating)`, where `first` is `firstwin` (a non-float).
  For a float `curwin`, `firstwin != curwin`, so it returns false and
  `CTRL-W T` proceeds. Floats and external windows both have `w_floating`.
  Verified for a normal float: `wincmd T` succeeded, the float id became
  invalid, and a new normal window opened in a new tab (4 tabs afterwards).
  External-window case, by reading only: `win_new_tabpage` -> `leave_tabpage`,
  and `tabpage_check_windows(old_curtab)` (`:4567`) moves the external window to
  the new tab's window list; going back with `goto_tabpage_tp(oldtab)` moves it
  back again (`:4787`). Whether `curwin == wp` still holds so the external window
  gets closed was not traced or run.
- Tab switch and floats: `tabpage_check_windows`, `window.c:4844-4872`, called
  from `enter_tabpage` (`:4787`) and `win_new_tabpage` (`:4567`):
  - for floats in the old tab that are not external: `ui_comp_remove_grid`
    (`:4858`), i.e. removed from the screen. They stay in the old tab's
    window list;
  - for floats in the new tab that are not external: `win_config_float` again
    (`:4864-4867`), re-adding them;
  - for external floats: `win_remove(wp, old_curtab); win_append(lastwin_nofloating(NULL), wp, NULL)`
    (`:4855-4857`), i.e. moved to the new current tab. The comment `:4846-4847`:
    "External floats are considered independent of tabpages. This is
    implemented by always moving them to curtab."
  - Related: when an external window is closed or turned into a normal
    window, other tabs' `tp_curwin` pointing at it are reset to
    `tp_firstwin` (`window.c:1405-1411`, `:2999-3006`).
  Verified for normal floats: a float opened in tab 3 was absent from
  `nvim_tabpage_list_wins(0)` after `tabfirst`, still valid, and
  `nvim_win_get_tabpage(float)` was tab 3.
- Not verified locally: any external window behaviour (needs `ext_multigrid`).

## 4. AeroSpace (low priority)

### Short answer

Yes: `aerospace move-node-to-workspace --window-id N <name>` works for a name
that doesn't exist yet. Read only; AeroSpace.app is not running here, so it was
not run.

### Evidence

- Docs, `workspace-back-and-forth`: "Workspaces are name-addressable objects.
  They are created and destroyed on the fly."
  <https://nikitabobko.github.io/AeroSpace/commands#workspace-back-and-forth>.
  `move-node-to-workspace` signature (with `--window-id <window-id>` and
  `<workspace-name>`) is at
  <https://nikitabobko.github.io/AeroSpace/commands#move-node-to-workspace>.
- Source (main `0921fb81`): `Sources/AppBundle/command/impl/MoveNodeToWorkspaceCommand.swift:25`
  calls `Workspace.get(byName: name.raw)`. `Sources/AppBundle/tree/Workspace.swift:22-30`
  returns the existing workspace or creates one and stores it in
  `workspaceNameToWorkspace`.
- Caveat: "destroyed on the fly" means an empty, non-visible workspace
  disappears again. A window moved there keeps it alive (and `--focus-follows-window`
  makes it visible). The `next|prev` forms do not create; only the name form does.
