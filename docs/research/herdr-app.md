# The Herdr app and its hc core

Facts for the `herdr-app` pane provider (SPI-1213), re-run on current
versions for SPI-1214. Checked on 2026-11-16 against **hc 0.1.21** (installed
into a scratch folder) and **Herdr app 0.1.16** (`/Applications/Herdr.app`),
macOS arm64, in a private sandbox (below), never the user's own window or core.
The first round (hc 0.1.17) is in SPI-1213's "Facts found"; where 0.1.21
differs, it says so. The recorded replies are in
`tests/fixtures/herdr_app/`, named in brackets below.

## Sandbox

1. Read the install script, then install hc into a scratch folder:
   `HC_INSTALL_DIR=/tmp/spi1214/bin sh install.sh` (from
   `https://releases.herdr.dev/core/install.sh`). Outside `$HOME` it copies a
   plain `herdr-core` there, starts no core, and leaves `~/.local/bin` alone; it
   adds no `hc` link when one is already on PATH, so link
   `hc -> herdr-core` yourself. Link `herdr-app` to
   `/Applications/Herdr.app/Contents/MacOS/Herdr` there too (the CLI is the
   app binary, chosen by its name).
2. Environment: `PATH=/tmp/spi1214/bin:$PATH`, `HC_SESSION=spi1214`,
   `XDG_STATE_HOME=/tmp/spi1214/state`, `XDG_CONFIG_HOME=/tmp/spi1214/config`,
   `HERDR_APP_HOME=/tmp/spi1214/apphome`, and unset `HC_TERMINAL_ID`,
   `HC_TERMINAL_SOCKET`, `HC_SCOPE`. The core's socket is
   `$XDG_STATE_HOME/herdr-core/$HC_SESSION/$HC_SESSION.sock`.
3. `$HERDR_APP_HOME/settings.json`: `{"core": "/tmp/spi1214/bin/herdr-core"}`.
   `$HERDR_APP_HOME/shared.json`: `{"frame.welcomed": "skipped",
   "workspace.panels": []}`, or the welcome overlay covers the window (it
   ignores `overlay.close`) and the Files and Changes panels take its width.
4. Run `/Applications/Herdr.app/Contents/MacOS/Herdr` in the background with
   that environment. Its window opens on the focused screen: poll
   `aerospace list-windows --monitor all --pid <pid>` for it, move it to a
   workspace no monitor shows (`aerospace move-node-to-workspace --window-id W
   spi1214`), then show each monitor's previously visible workspace again, the
   focused one last. `herdr-app shot FILE` draws the hidden window to a PNG.
5. When done: `herdr-app do app.quit`, then `hc server stop` (with the app
   gone, or it starts the core again), and check that nothing under the
   sandbox runs.

Surprise: any `hc` call that needs a core starts one, `hc schema` included, so
set the environment before the first `hc`. A core started from this shell
passes `XDG_*` and `HERDR_APP_HOME` on to its terminals.

## Facts

### Environment and identity

- An app terminal's environment: `HC_TERMINAL_ID` (`t1f`),
  `HC_TERMINAL_SOCKET` (the core's socket), `HC_SESSION`, `HC_SCOPE`,
  `TERM_PROGRAM=ghostty`, `TERM=xterm-256color`. Still no
  `HERDR_SOCKET_PATH`. Found by `pane.split` running `env` into a file.
- `hello` answers `generation: 1`, `version`, `features` (`framed`, `attach`,
  `events`, `status`, `shell`, `resize_notifications`, `automatic_input`,
  `restart_environment`), `methods` (every one `hc schema` lists) and
  `core {id, name}` [`core/hello`].
- Terminal addresses go `t1`…`t9`, `t1a`, `t1b`…: opaque strings, not
  `t<number>`.
- The doc's tab `root` has the tab's own id (`tb…`); splits are `s…`, panes
  `p…`, spaces `sp…`.

### `herdr-app`: replies and latency

- Every `do` answers one JSON object on stdout: `{ok: true}`, the ids it made,
  or `{error}` with exit 1 [`app/err-*`]. Unknown argument names fail and list
  the valid ones [`app/err-unknown-argument`]; a non-string target id fails
  [`app/err-target-not-string`]. With no window under `HERDR_APP_HOME`, every
  command exits **2** with nothing on stdout and
  `herdr-app: no Herdr window at <home>/herdr-app.sock: …` on stderr
  [`app/err-no-window-*`].
- `pane.split {direction, pane, argv}` answers `{pane, terminal}` in 30–40 ms;
  the new pane takes focus, also in an unshown tab, without showing that tab
  [`app/do-pane.split*`].
- `tab.new {argv, focus: false, space}` answers `{space, tab, terminal}` (no
  pane id) in ~30 ms and keeps the shown tab [`app/do-tab.new`].
- **Changed since 0.1.17: `tab.new` and `terminal.open {place: "tab"}` run
  `argv` through the login shell and keep a shell after it:** the terminal's
  argv is `[$SHELL, -lic, "\"$@\"; exec \"$0\" -li", $SHELL, …argv]`. When the
  program exits, the tab stays, now holding an interactive login shell
  (checked with `sh -c 'sleep 2.5; exit 0'`: 2 s later the terminal is live,
  foreground `zsh -li`, its block still in the doc). `pane.split` and
  `terminal.open {place: "right"}` run `argv` bare, and their pane closes when
  it exits. A tab whose only pane closes goes too. So a tab made by
  `tab.new {focus: false}` with no `argv`, then `pane.split` with `argv` into
  its pane, then `terminal.kill` of the tab's first shell, leaves a tab of the
  bare program that closes when it exits; the shown tab and focused pane
  don't change.
- `pane.swap {pane, direction}` swaps with the neighbour and focus stays with
  `pane`; at an edge it answers `ok` and nothing moves [`app/do-pane.swap*`].
  `direction` and `target` together fail.
- `pane.go` moves to the neighbour **most recently focused**, not the one
  nearest the cursor: from `t1` in `[t1 | (t2 / t3)]`, `right` lands on
  whichever of `t2`, `t3` had focus last (three tries, alternating). At an edge it
  answers `ok` and focus stays [`app/do-pane.go-edge`].
- `pane.focus` of a pane in an unshown tab selects that tab.
- `herdr-app status`: `{zoom, font_size, ui_font_size, space, tab,
  focused_pane, layout, agent, floats, overlays, errors}`; no rects, no zoomed
  pane [`app/status*`]. A toast shows in `overlays` as `"toast"`.
- `layout.resize {id, weight}` sets that node's `size` to `{weight}` (as an
  f32: 0.7 reads back as `0.699999988079071`), on any node, panes, splits, and
  an unshown tab's; `{id, px}` writes `{px}`; `{id}` alone answers `ok` and
  changes nothing; an unknown id fails `no node "…"` [`app/do-layout.resize*`].
- **Latency** (the window hidden, not key; each call a new `herdr-app`
  process, 20 sequential calls): `pane.go` median 9.2 ms, max 10.9 ms, all 20
  in 191 ms; `pane.focus` median 16.2 ms, max 28.5 ms, 331 ms; `status` median
  8.5 ms; `ls` median 8.6 ms. 20 × `pane.go` fits 2 s ten times over, and no
  call came near 75 ms, so `focus_direction` can be COUNT × `pane.go`. (0.1.17
  measured ~23–27 ms per call.)

### The core's `doc.get herdr-app` and the tree shapes

- spaces → tabs → `root` (`row`/`column`) → `block {id, content: {terminal},
  size?}` [`core/nested-doc.get`]. Unchanged from 0.1.17.
- Where `size` goes after a split, found by splitting in a second tab and
  reading the doc after each [`core/shapes-doc.get`]:
  - Splitting **along** the parent's axis halves the origin's weight between
    the origin and the new block, both written (`[t4@0.5 | t5@0.5]` →
    `[t4@0.5 | t5@0.25 | t6@0.25]`); an origin with no `size` counts as 1 and
    both get 0.5.
  - Splitting **across** it wraps the origin in a new split that takes the
    origin's `size`; the two blocks inside have none:
    `t6@0.25` → `(t6 / t7)@0.25`.
  - So `[t4 | t5 | (t6 / t7 / [t8 | t9])]` reads
    `t4@0.5, t5@0.25, (t6, t7@0.5, [t8, t9]@0.5)@0.25`: `t6` has no `size`
    (weight 1) and gets half the column.
- When a block leaves, its weight goes to a neighbour, so the sum stays
  (`t5@0.5` + a closed `@2` → `t5@2.5`), and a split left with one child is
  replaced by it, the child taking the split's `size`.
- The app's own guide (`$HERDR_APP_HOME/AGENTS.md`): weights are "shares of
  what fixed siblings leave", so `px` siblings come off first. People can also
  drag panes by a grip to another place in the tab (`movable`, default true),
  which reshapes the tree.

### Cols and rows

- `terminal.list`'s entries are field for field the records `terminal.get`
  answers for the same terminals (all 9 compared) [`core/nested-terminal.*`].
  `terminal.list` also has `hidden_finished` and `machines`.
- The nested tab `[t1 | (t2 / t3)]`, weights 0.5/0.5 and no sizes in the
  column: `t1` 39×52, `t2` 39×24, `t3` 39×24.
- **Pane chrome is about 5 columns and 4 rows per pane** (border, a two-row
  header, padding and gap) at font size 14, zoom 1, and weights share the
  panes' **outer** extent, chrome included. With outer = cells + (5, 4), every
  measurement fits exactly or within rounding: `[t4@.5 | t5@.25 | (…)@.25]`
  outer widths 43 + 22 + 22; the column's outer heights 28 + 14 + 14 = 56 =
  `t4`'s 52 + 4; `[t8 | t9]` 11 + 11 = 22 = `t7`'s 17 + 5; weights 3:1 in a
  column gave 38 and 10 rows (outer 42:14); weights 0.5:1.5 gave 17 and 60
  cols (outer 22:65). Cells alone undercount a split by one pane's chrome per
  extra pane along its axis (`t2` + `t3` rows = 48, `t1` = 52; `t8` + `t9`
  cols = 12, `t7` = 17), so "about one cell off" in the design is about 4 rows
  or 5 columns off per pane. The chrome is in pixels, so its size in cells
  changes with font size and zoom. (0.1.17's 38 / 17 / 17 fits the same
  4 rows.)
- A terminal in a tab never shown is **120×40** (the core's default; the
  `created` event even says 200 cols first), whatever the window: a split in
  a never-shown tab gives both panes 120×40 until the tab is shown. Once its
  tab has been shown, a terminal keeps its last real size while the tab is
  hidden. A split into a hidden tab that had been shown resized its
  neighbours at once but left the new terminal at 120×40 until a
  `layout.resize` in that tab [`core/events.subscribe`]: don't trust a hidden
  tab's cols/rows.
- **Zoom**: the zoomed pane's terminal takes the whole tab (`t2` 39×24 →
  82×52); the others keep their unzoomed sizes, shown tab or not
  [`core/zoomed-doc-and-list`]. Neither the doc nor `status` shows the zoom;
  `$HERDR_APP_HOME/view.json` does, `tabs.<tab>.zoomed: "<pane>"`, written at
  once (0.1.17 took up to ~3 s). `view.json` also keeps each tab's `focused`
  pane and `before`, its earlier focused panes.
- `pane.split` into a zoomed tab unzooms it and focuses the new pane
  [`app/do-pane.split-zoomed`].

### Focus

- Focus reports (mode 1004) reach a pane's program on `pane.focus`,
  `pane.go` and `tab.select`, with the window hidden and not key: `^[[I` in the
  newly focused pane, `^[[O` in the one left [`core/focus-report-terminal.read`].
  Over 20 `pane.go` and 20 `pane.focus` calls between `t2` and `t3`, each
  pane's screen read `^[[I^[[O^[[I…`: one report per move, none for a
  `pane.go` at an edge.
- `events.subscribe` sends nothing on a focus change [`core/events.subscribe`].
- **Not settled: a mouse click.** Clicking a pane needs a pointer event on the
  window; agent-desktop's headless (accessibility) actions have nothing to
  press in a terminal pane, `--headed` input takes the user's pointer and needs
  the window on screen, and this run's accessibility access was refused
  (`PERM_DENIED`). Left for a person.
- **Not settled: dragging a divider** (that it writes weights, never px), for
  the same reason. The guide says dividers are draggable (`resizable`,
  default true) and that `size` is `{px}` or `{weight}`.

### Kill, keep and closing

- `terminal.kill` of a live terminal answers its finished record
  (`reason: killed`, `signal: 1`, `exit_code: null`, no `foreground`); again,
  the same record; an unknown address, error `-32000`, `data.kind: not_found`,
  with a `data.hint` [`core/terminal.kill*`].
- The core's `terminal.keep {keep}` answers the record with `keep` set; a
  finished terminal gives `-32602 invalid_params` ("only live terminals can be
  kept"); `not_found` as above [`core/terminal.keep*`].
- **`terminal.kill` of a pane running `neopane attach`** (a daemon started
  with `--detach` under a private `NEOPANE_TMPDIR`, attached in a split): the
  kill answered in 17 ms, the block left the doc within 0.6 s, its sibling
  took its weight, no toast (`status.overlays` stayed empty, and the window
  showed none), and the daemon logged `the last pane went; asking Neovim about
  unsaved changes` then `kept, staying unattached`: a hang-up, not an error.
  The attach itself dies of SIGHUP and logs nothing.
- A program that exits nonzero within 2 s of starting shows a toast
  ("Terminal failed to start", "Exit code 3"), listed in `status.overlays`
  for 5–13 s. A terminal killed 0.1 s after `pane.split` shows none: a kill
  ends it by signal, with no exit code.
- After `hc server stop`, a keep-off terminal finishes with
  `reason: server_stop` and its block leaves the doc; keep-on terminals come
  back as fresh login shells in place of their programs
  [`core/after-core-restart`]. Unchanged from 0.1.17. New: a running app
  starts its core again within a second (its `hc connect` relaunches it), so
  the core is only down for good once the app has quit.

### Events

- `events.subscribe` answers a snapshot (`channel`, `seq`, `terminals`), then
  `event` notifications `{seq, type, terminal?, data}` and a `heartbeat
  {seq}` every 15 s [`core/events.subscribe`]. Types seen: `created` (the
  full record), `doc {name, version}`, `size {cols, rows}`, `foreground`,
  `labels`, `keep {keep}`, `finished {exit_code, finished_ms, reason,
  signal}`. A split brings `created`, `doc`, `size`, `foreground`, `labels`;
  a resize `doc` and a `size` per terminal that changed; a kill `finished`,
  then the neighbours' `size`, then `doc`.

### Install and the app

- hc installs on Linux and macOS with `install.sh` (`HC_VERSION`,
  `HC_INSTALL_DIR`); 0.1.21 is the latest release today. Under `$HOME` on
  macOS it installs a signed `Herdr Core.app` bundle instead and restarts a
  running core on update.
- `herdr-app --help`: `ls`, `do`, `feedback`, `commands`, `status`, `shot`,
  `check`, `--skill`, `--version`; home `~/.config/herdr-app` unless
  `HERDR_APP_HOME`. `commands` works without a window [`app/commands`].
- Launching the private app left `~/.local/bin/herdr-app` untouched (its
  link already pointed at the app).
