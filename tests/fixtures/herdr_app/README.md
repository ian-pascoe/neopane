# Herdr app fixtures

Replies recorded from a private hc core and Herdr app window (SPI-1214), for
the fake core socket and fake `herdr-app` in the `herdr-app` pane provider's
unit tests. The facts they show, how each was found, and the sandbox recipe:
`docs/research/herdr-app.md`.

Recorded on 2026-11-16 with **hc 0.1.21** (`hc --version`; `hello` answers
`version: "0.1.21"`, `generation: 1`) and **Herdr app 0.1.16**
(`herdr-app --version`), macOS arm64, window 1016×1110 pt, font size 14,
zoom 1. CI installs this hc, pinned in `scripts/install-hc.sh`, for the
live core tests (`tests/herdr_app.rs`): move the pin and the fixtures
together. `app/version.json`, `core/hc-version.json`,
`core/server.status.jsonl`, `core/terminal.kill-attach.jsonl` and
`app/status-after-kill-attach.json` come from a second session the same
day, in the same sandbox after the app and core were restarted; the rest
from the first.

Every value is as the core or app sent it, except three scrubbed strings: the
core id is `0123456789abcdef0123456789abcdef`, the machine name `host`, the
user name `user`. Ids (`sp…` space, `tb…` tab, `s…` split, `p…` pane) are the
app's own; terminal addresses are the core's (`t1`…`t9`, then `t1a`, `t1b`:
treat them as opaque strings).
"Space" and "tab" are the Herdr app's words, not GLOSSARY's **herdr
workspace** and **herdr tab** (the herdr multiplexer's).

## `core/*.jsonl`: JSON-RPC exchanges with the core

One JSON value per line, in wire order: `{"send": <request>}` for a line
written to the socket and `{"recv": <reply or notification>}` for a line read
from it. Every file starts with a `hello`. Most are a whole connection, ids
from 1. Ten are slices of a longer connection, cut at a `hello` sent again
mid-connection (the core answers a repeated `hello` as the first), so their
ids start higher: `nested-doc.get` (7), `nested-terminal.list` (5),
`terminal.get-finished` (8), `terminal.get-not-found` (6),
`terminal.keep-finished` (6), `terminal.keep-missing-param` (8),
`terminal.keep-not-found` (4), `terminal.kill-not-found` (4),
`doc.get-unwritten` (10) and `unknown-method` (12). A fake core can replay a
file by answering each `send` with the `recv` lines that follow it, up to the
next `send`, with the reply's `id` set to the request's own `id`: echo the
ids the client sends, don't count them from 1.

The **nested tab** is `[t1 | (t2 / t3)]` (the tab SPI-1213's "Facts found" measured pane chrome in, `[t1 | (t9 / t10)]` there):
a row holding `t1` (weight 0.5) and a column (weight 0.5) of `t2` over `t3`,
neither with a `size`, as `nested-doc.get` reads it. A `layout.resize` test
afterwards left `t2` with `size: {weight: 1.0}` (`t3` still none, so the same
1:1 split), so every later `doc.get` of that tab reads `t2@{weight: 1.0}`:
`shapes-*`, `zoomed-doc-and-list`, `after-core-restart` and
`terminal.kill-attach`.

| File | What |
| --- | --- |
| `hello.jsonl` | `hello`: generation, features, methods, version. |
| `server.status.jsonl` | `server.status`: the core's `version`, `pid`, uptime, holder and terminal counts. |
| `nested-doc.get.jsonl` | `doc.get {name: "herdr-app"}` of one space with the nested tab. |
| `nested-terminal.get.jsonl` | `terminal.get` of `t1`, `t2`, `t3` in the nested tab: 39×52, 39×24, 39×24. |
| `nested-terminal.list.jsonl` | `terminal.list` at the same moment: the same three records. |
| `shapes-doc.get.jsonl` | Six `doc.get`s in a second tab: before, then after each of four splits, then once more (the same as the fifth): `[t4 \| t5]`, `[t4 \| t5 \| t6]`, `[t4 \| t5 \| (t6 / t7)]`, `[t4 \| t5 \| (t6 / t7 / t8)]`, `[t4 \| t5 \| (t6 / t7 / [t8 \| t9])]`. Shows where `size` is missing. |
| `shapes-doc-and-list.jsonl` | `doc.get` and `terminal.list` with that second tab shown: real cols/rows of `t1`…`t9` (`t4` 38×52, `t5` 17×52, `t6` 17×24, `t7` 17×10, `t8` and `t9` 6×10). |
| `zoomed-doc-and-list.jsonl` | `t2` zoomed in the nested tab: `terminal.get` of `t1`…`t3` while shown (`t2` 82×52, the others unchanged), again with the tab hidden (unchanged), then `doc.get` (no trace of the zoom) and `terminal.list`. |
| `terminal.get-finished.jsonl` | `terminal.get` of a killed terminal: `state: finished`, `reason: killed`, `signal: 1`, no `foreground`. |
| `terminal.get-not-found.jsonl` | `terminal.get` of `t99`: error `-32000`, `data.kind: not_found`. |
| `terminal.list-all.jsonl` | `terminal.list {all: true}`: live and finished. |
| `terminal.kill.jsonl` | `terminal.kill` of a live terminal (its finished record), then again (the same record). |
| `terminal.kill-not-found.jsonl` | `terminal.kill` of `t99`: `not_found`. |
| `terminal.kill-attach.jsonl` | A tab `[t11 \| t1g]`, `t1g` running `neopane attach` in an unshown tab: `doc.get` (`t1g`'s block there), `terminal.kill` of `t1g` (finished, `reason: killed`, `signal: 1`, SIGHUP), then 1 s later `doc.get` (its block gone, `t11` alone at weight 1). |
| `terminal.keep.jsonl` | The core's `terminal.keep {keep: false}`, then `{keep: true}`. |
| `terminal.keep-not-found.jsonl` | `terminal.keep` of `t99`: `not_found`. |
| `terminal.keep-finished.jsonl` | `terminal.keep` of a finished terminal: `-32602`, `invalid_params`, "only live terminals can be kept". |
| `terminal.keep-missing-param.jsonl` | `terminal.keep` without `keep`: `invalid_params`. |
| `doc.get-unwritten.jsonl` | `doc.get` of a document never written: `value: null`, `version: 0`. |
| `unknown-method.jsonl` | An unknown method: `-32601`, `data.kind: unknown_method`. |
| `events.subscribe.jsonl` | `events.subscribe`: the snapshot reply, then the notifications from a `pane.split` (`created`, `doc`, `size`, `foreground`, `labels`), a `layout.resize` (`doc`, `size`), a `pane.focus` (nothing), `terminal.keep {keep: false}` (`keep`) and `terminal.kill` (`finished`, `size`, `doc`). |
| `focus-report-terminal.read.jsonl` | Panes `t2` and `t3` run `printf '\e[?1004h'; cat -v`, `t3` focused. `terminal.read` of `t2` before `herdr-app do pane.focus` of `t2` (empty), then twice after (`^[[I`), then of `t3` after only (`^[[O`; it wasn't read before). |
| `after-core-restart.jsonl` | After `hc server stop` (the app restarts the core at once): a keep-off terminal finished with `reason: server_stop`; keep-on ones are fresh login shells; `doc.get` without the keep-off block. |

`hc-version.json` is `hc --version` (`hc 0.1.21`), kept in the same
`{argv, exit, stdout, stderr}` shape as the `app/` files, with `stdout` the
plain text. `hc-schema.json` is `hc schema` (OpenRPC):
every method with its params, result and errors.

## `app/*.json`: `herdr-app` replies

One run each: `{"argv", "exit", "stdout", "stderr"}`, `stdout` parsed as JSON
(null when empty). A fake `herdr-app` can match `argv` and print `stdout`
(pretty-printed, as the app does), write `stderr` and exit with `exit`.

| File | What |
| --- | --- |
| `status.json` | `status` with the nested tab shown: `focused_pane`, `tab`, `space`, no rects. |
| `status-zoomed.json` | `status` with `t2` zoomed: the same shape; the zoom is not in it. |
| `status-after-kill-attach.json` | `status` right after `core/terminal.kill-attach`: `overlays` empty, no toast. |
| `ls.json` | `ls` with the nested tab: spaces → tabs → terminals with their pane ids. |
| `commands.json` | `commands`: every command, its arguments, what it does. |
| `version.json` | `--version`: plain text, `herdr-app 0.1.16`, kept as a string in `stdout`. |
| `do-space.new.json` | `space.new`: `{space, tab, terminal}`. |
| `do-pane.split.json`, `do-pane.split-down.json` | `pane.split` right and down with `argv`: `{pane, terminal}`. |
| `do-pane.split-zoomed.json` | `pane.split` into a zoomed tab (it unzooms). |
| `do-pane.swap.json`, `do-pane.swap-edge.json` | `pane.swap` up; and left at the window's edge (`ok`, nothing moves). |
| `do-pane.focus.json` | `pane.focus {pane}`: `{ok: true}`. |
| `do-pane.go.json`, `do-pane.go-edge.json` | `pane.go` right; and left at the edge (`ok`, focus stays). |
| `do-pane.zoom.json` | `pane.zoom {pane}`. |
| `do-tab.new.json` | `tab.new {argv, focus: false, space}`: `{space, tab, terminal}`, no pane id. |
| `do-tab.select.json` | `tab.select {id}`. |
| `do-layout.resize.json`, `do-layout.resize-px.json`, `do-layout.resize-nothing.json` | `layout.resize {id, weight}`, `{id, px}` (writes `size: {px}`), and `{id}` alone (`ok`, no change). |
| `err-unknown-argument.json` | An unknown argument: exit 1, `{error: "pane.split: unknown argument(s): bogus; valid arguments: …"}`. |
| `err-unknown-command.json` | An unknown command. |
| `err-unknown-pane.json` | `pane.focus` of an unknown pane: `no such pane`. |
| `err-target-not-string.json` | A target id that isn't a string. |
| `err-bad-direction.json`, `err-pane.go-bad-direction.json` | A bad `direction` for `pane.split` and `pane.go`. |
| `err-pane.swap-both.json` | `pane.swap` with both `direction` and `target`. |
| `err-layout.resize-unknown.json` | `layout.resize` of an unknown node: `no node "pnope"`. |
| `err-tab.new-unknown-space.json` | `tab.new` into an unknown space. |
| `err-no-window-status.json`, `err-no-window-do.json` | `status` and `do` with no window under `HERDR_APP_HOME`: exit 2, nothing on stdout, `herdr-app: no Herdr window at …` on stderr. |

## Recording more

`record.py` is the helper that recorded them (`Core` for the socket, `app` and
`do` for the CLI). Set up the sandbox in `docs/research/herdr-app.md` first,
then scrub the three strings above before committing.
