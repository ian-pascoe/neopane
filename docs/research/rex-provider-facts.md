# Rex API facts for a pane provider

Facts gathered while designing Rex as one "pane provider" behind a generic
interface (R1 to R6). Checked on 2026-11-09 against Rex server
`0.0.1793981048-g01d6c4` (`server.version`: `api_revision` 11, commit
`01d6c4fe`, built 2026-11-06). neorex's client sends `api_revision` 7 in its
`hello` (`src/rex/client.rs:16`); the server accepted it and answered with
`api_revision` 11.

## Sources and method

- **Docs.** The server API reference, <https://www.superlogical.com/rex/docs/reference/api.md>
  (all of it is also in <https://www.superlogical.com/rex/docs/llms-full.txt>),
  and the server's own per-method docs, `rex api describe METHOD --json`,
  which the Server API page says always match the connected server
  (<https://www.superlogical.com/rex/docs/automate/server-api.md>). Below,
  "docs" means one of these; the method is named.
- **Not consulted: the `mcp__superlogical` MCP server and Rex's changelog.**
  That server (the Rex community MCP, which also serves the changelog) is not
  among the tools this agent was given: `searchTools`, `describeNamespace`
  and `ALL_TOOLS` found nothing for `superlogical` or `rex`, and the site has
  no public changelog page (`/rex/changelog` and `/rex/docs/changelog.md` are
  404). So no claim below rests on release notes.
- **Observed live.** A throwaway Python WebSocket client (stdlib only; in
  `/tmp/rexprobe/`, not committed) spoke `rex.control.v1` over the server's
  Unix socket, as neorex does: `GET /api/control`, a `hello`, then
  `{"type":"request","id":"N","method":...,"payload":...}`. Every experiment
  ran in a scratch session made with `rex new rex-facts-scratch`, which was
  never selected in Rex.app, and was removed with `rex kill` at the end. The
  user's sessions were not touched. The scratch session's blocks ran
  `/bin/cat` or `sleep` with `"shell":"none"`, as neorex's panes do.
- Response and event envelopes seen:
  `{"type":"response","id":"3","seq":1,"payload":{...}}`,
  `{"type":"response","id":"1","seq":1,"error":{"code":"not_found","message":"...","status":404}}`,
  `{"type":"event","seq":2,"payload":{"type":"session_event","payload":{"session_id":"session:...","event":{"type":"session_view_changed","payload":{"revision":994}}}}}`.

## R1. Does creating a pane move focus? Does `disown_block`?

**No, to all three, unless `focus: true` is sent.**

Docs (`session.new_split`, `session.new_layer` input schemas): `focus` is
"Whether to make the anchor's window active and focus the first new block.
Defaults to false." (`new_layer`: "...make the window active and focus the
first new block. Defaults to false.") The CLI's "new panes and tabs take
focus" (rex skill guide) is the CLI's own default; the API's is false.
`session.disown_block` (docs) only gives up the caller's ownership; it names
no focus change. Its docs say that if a block in a window changes owner "the
call also sends one `session_view_changed`".

Observed, with neorex's exact payloads (the `neorex` label, `shell:"none"`,
`size` option; `anchor_id`, `direction`, `side`, `layout`, no `focus`; for the
layer, `bounds` and `layout`, no `focus`):

| Step | `active_window_id` | focused block |
| --- | --- | --- |
| before | W | `…100d0b` (the anchor) |
| `new_split` (no `focus`) → new block `…e52a99` | W | `…100d0b` |
| `disown_block` of `…e52a99` | W | `…100d0b` |
| `new_layer` (no `focus`) → new block `…0ec09d` | W | `…100d0b` |
| `disown_block` of `…0ec09d` | W | `…100d0b` |
| `new_split` with `"focus":true` → new block `…204bb9` | W | `…204bb9` |

Responses:
`new_split` → `{"anchor_id":"block:…100d0b","block_ids":["block:…e52a99"],"split_ids":["split:…cf57"],"revision":2}`;
`new_layer` → `{"layer_id":"layer:…b66332","block_ids":["block:…0ec09d"],"revision":4}`;
`disown_block` → `{"blocks":[{"block_id":"block:…e52a99"}]}` (no `owner`: the block had no owner after it).

Across windows (two windows; W2 active; neither call sent `focus`):
a `new_split` whose `anchor_id` is in non-active window W1, and a `new_layer`
with `window_id` = W1, left `active_window_id` at W2 and W1's focused block
unchanged. With `"focus":true` on a `new_layer` into W1, `active_window_id`
became W1 and W1's focused block became the new block.

Events: creating a block sends `block_created`, then `block_owner_changed`
(`"reason":"create"`, owner = the creating connection's client id), then
`block_event` (`process_changed`). A focus change shows only as
`session_view_changed` with a higher `revision` (seen for `session.focus_block`
and `new_split` with `focus:true`); there is no dedicated focus event.

Caveat: this tests the server, not the app. What Rex.app does on its own when
a block appears was not observable, because the scratch session was not shown.

## R2. A floating layer's blocks in `session.view`

**A block in a floating layer has a `rect` (in the layer's `layout`), in
fractions of the Rex window, not of the layer. Layer `bounds` are in the same
window fractions. Whether the data includes the layer's frame could not be
determined headlessly; the data carries no frame.**

Docs (`session.view` output schema): `layers[].bounds` — "Where the layer
sits, as [x, y, w, h] fractions of the window's width and height";
`layers[].layout.rect` — "Where this node sits, as [x, y, w, h] fractions of
the window's width and height"; `layers` are "from back to front. The first is
the tiled layer". `new_layer` validation note: "`bounds` is measured from the
window's top-left corner." Nothing in the docs mentions a frame or border
around a floating layer.

Observed. A layer made with `bounds` `[0.1,0.1,0.5,0.5]` and one block:

```json
{"layer_id":"layer:01a1c034-aa17-79fe-9858-079b5db66332","kind":"floating",
 "bounds":[0.1,0.1,0.5,0.5],
 "layout":{"rect":[0.1,0.1,0.5,0.5],"block_id":"block:01a1c034-a9dd-7a5c-9d8d-190dd40ec09d"},
 "blocks":[{"block_id":"block:01a1c034-a9dd-7a5c-9d8d-190dd40ec09d","label":"neorex",
            "creator_name":"com.superlogical.terminal","flavor":"com.superlogical.terminal.shell",
            "host_id":"95ab77c0-cd84-4956-93e6-8a5aa2dfd499"}]}
```

A layer with a horizontal split (`ratio` 0.25) at `bounds` `[0.2,0.3,0.6,0.4]`:

```json
{"kind":"floating","bounds":[0.2,0.3,0.6,0.4],
 "layout":{"rect":[0.2,0.3,0.6,0.4],
   "split":{"split_id":"split:01a1c035-51c1-7cfc-9c4a-3d8f3ad1491a","direction":"horizontal","ratio":0.25,
     "before":{"rect":[0.2,0.3,0.15,0.4],"block_id":"block:…7944"},
     "after":{"rect":[0.35,0.3,0.44999999999999996,0.4],"block_id":"block:…cd14"}}}}
```

After `session.resize_layer` to `[0,0,1,1]` the rects became
`[0,0,1,1]`, `[0,0,0.25,1]` and `[0.25,0,0.75,1]`. So:

- A layer's root `layout.rect` equals its `bounds` exactly, and its child
  rects partition `bounds` by `ratio` with no gap. Neither is shrunk by a
  frame. Whole numbers come as integers (`0`, `1`), others as floats
  (`rect4` in `src/daemon/rex_layout.rs` already reads both).
- `bounds` covers the whole layer as Rex was asked to place it. The tiled
  layer reads `"bounds":[0,0,1,1]`.
- The `rect` is not the terminal's character area if Rex.app draws a header,
  border or padding inside it. **Not determined:** how many cells that takes.
  Reaching it needs a block's terminal size (`com.superlogical.terminal.size`)
  in a session a Rex.app window is showing, compared with the window size
  (the size of a full-window tiled block); the scratch session is not shown
  by design, so its blocks keep the size the creator asked for (`40x10`, or
  `80x24` by default). The docs say pane headers and spacing are user
  settings (Settings > Interface > Panes: "Show pane headers", spacing), so
  the frame is probably not a constant; `LAYER_FRAME = [3, 2]` in
  `src/daemon/floating_panes.rs` is a guess, as its comment says.

## R3. Anything that proves which terminal a process runs in?

**No tty path anywhere. Pids exist, only through `com.superlogical.terminal.process`
and its `process_changed` event, and on macOS they are `login`'s, not the
program's.**

- `session.view` / `session.attach` blocks have only `block_id`, `label`,
  `creator_name`, `flavor`, `host_id`, `owner` (docs; seen live).
  `session.list_blocks` and `session.describe_block` add `placement` and
  `window_id`. `describe_block` also returns the method and event schemas
  (~50 KB). `target.resolve` gives `kind`, ids, `label`, `where`. None has a
  pid or tty. (Observed.)
- `com.superlogical.terminal.process` (docs: "Get the processes running in the
  terminal and how its program exited") returns `last_exit`, `child`,
  `foreground`, `ancestors`, `home`. Each process has `pid`, `user_id`,
  `user`, `name`, `path`, `invoked_path`, `argv0`, `script`, `cwd`; no tty.
  `child` is "the process the terminal started... On macOS a login shell runs
  under login, and this describes login". `foreground` is the foreground
  process; `ancestors` lies between them (nearest first, ending with `child`;
  left out when `foreground` is `child`, when more than 16, or when one has
  exited). The `process_changed` block event (`generation`, `foreground`,
  `ancestors`) pushes the same data. `title`, `pwd`, `size`, `modes`,
  `input_mode`, `program_status` have no pid/tty either (all called; `size`
  has `columns`, `rows`, `cell_width_px`, `cell_height_px`, `owner`, `desired`).
- Observed with neorex-style `shell:"none"` and `command:["sh","-c","echo $$ > pid; tty > tty; exec sleep 1000"]`:
  the program wrote pid `58007` and tty `/dev/ttys024`. Rex reported
  `child` = `foreground` = `{"pid":58005,"user":"root","name":"login","path":"/usr/bin/login"}`.
  `ps` showed `58005` is `login -q -flp … exec -a "-$name" -- "$@" rex-login sh /bin/sh -c …`
  (pgid 58005, on `ttys024`) and `58007` (`sleep 1000`, ppid 58005) is in its
  process group. So even with `shell:"none"` the program is a child of
  `login` on macOS, and neither `child` nor `foreground` is the program's pid.
  For a default login shell the same call gave `child` = `login` 97169,
  `foreground` = `zsh` 97174, `ancestors` = `[login]`.
  (Linux behaviour was not tested.)
- `session.list_clients` / `client.list` report, for each connection, a
  `principal` with `uid`, `gid` and `pid` of the connecting process (e.g.
  `{"kind":"peer","transport":"unix","uid":501,"pid":31635}`) and `info`
  (`kind`, `name`, `version`). That is the control client's pid, not a
  terminal's; it identifies who is connected to Rex, not a block.
- What a provider can do: take its own process's ancestry (getppid chain) and
  match it with `child.pid`, `foreground.pid` or `ancestors[].pid` (on macOS
  its parent is `child.pid`), or compare the controlling tty of the `child.pid`
  process with its own (both were `ttys024` above). The tty comparison is
  derived from the `ps` output, not offered by Rex. It would be best to use
  with `REX_BLOCK` as the lookup key and the pids to verify it.

## R4. Are one connection's requests handled in order?

**No. Docs promise it only for some methods, and live, several layout/label
methods and all reads are not applied or answered in order.**

Docs (`client.action`, `client.key`, `block.event`): "On each connection,
`client.key`, `client.action`, `block.event`, and the methods that change
focus or the layout run one at a time, in the order they arrive." It adds:
"`block.method` doesn't wait for earlier requests on the same connection...
If a call depends on an earlier request, wait for that request's reply
first." `session.attach` documents events ordered by `seq` with no gaps. No
doc states that responses come in request order.

Observed (one connection, requests sent back to back without waiting; `ids`
counted in send order):

- **`session.resize_pane`**: 5 sends with ratios 0.2, 0.3, 0.4, 0.6, 0.7: the
  responses came as ids `[6,4,5,7,8]` with `revision`s `[15,16,17,18,19]`, so
  ratio 0.4 was applied before 0.2 and 0.3. With 30 sends (5 trials) the
  responses were out of order every time (71 to 131 inversions) and the final
  ratio was wrong every time (0.74, 0.35, 0.8, 0.89, 0.89 for an expected
  0.92). Sending them one at a time and awaiting each response ended at the
  right ratio (0.4).
- **`session.resize_layer`** (30 sends x 4) and **`session.set_block_label`**
  (30 x 4): responses out of order every time (93 to 208 inversions), final
  state wrong every time (the last-sent bounds/label did not win).
- **`session.focus_block`** (alternating 2 or 3 blocks, 30 x 4) and
  **`session.move_block`** (alternating `horizontal`/`vertical`, 30 x 4):
  responses in order (0 inversions) and the final state is the last one sent.
  `session.new_split` then `session.close_window` came back in order, both ok.
  (A `zoom_block`/`swap_blocks` run showed 0 inversions, but its checks could not
  tell success from failure; treat those two as untested.)
- **Reads run ahead of writes.** `new_layer`, `view`, `list_blocks` sent back
  to back were answered `12, 11, 10` (list_blocks, view, new_layer), and the
  `view` showed no new layer. Another run: 5 `resize_pane` then `view`; the
  view came last but at `revision` 14, before all five (ratio still `0.5`).
  So a `session.view` after a write can be from before it.

For a provider: wait for the reply to a `resize_pane`, `resize_layer` or
`set_block_label` before sending another that touches the same thing, and for a
write before a read that must see it. Back-to-back `focus_block` and
`move_block` happened to stay in order, in line with the docs. Only the
methods tried are covered.

## R5. `attach` response, `view` cost, cheap existence check

- **`session.attach` includes the full view.** Observed response keys:
  `client_id`, `view`; `view` is the same object `session.view` returns
  (`revision`, `session_id`, `label`, `active_window_id`, `windows[]` with
  `window_id`, `label`, `focused_block_id`, `layers[]`). So the active block is
  `view.windows[active].focused_block_id`, and no separate `session.view` is
  needed. The docs say the view is read after the connection started
  receiving events; "Apply `view` first, then each event whose `seq` is higher
  than the response's `seq`". (Observed: `"seq":0` on the attach response.)
  Attaching again is safe and returns a fresh view. `session.bootstrap` does
  attach, list sessions and start events in one call (docs).
- **Most session methods need an attached connection.** On an unattached
  connection `session.view`, `list_blocks`, `describe_block` and block methods
  fail `{"code":"conflict","message":"stream is not attached to session ..."}`;
  `target.resolve` worked without attach.
- **Round trips** (scratch session, 7 blocks, 1 window, local Unix socket, 200
  sequential calls each, wall time from send to response in the Python client):

  | Call | median | p95 | max | response |
  | --- | --- | --- | --- | --- |
  | `server.version` (floor) | 0.38 ms | 2.2 | 26.5 | 284 B |
  | `session.view` | 1.25 ms | 5.2 | 8.9 | 3.6 KB |
  | `session.list_blocks` | 0.73 ms | 2.9 | 6.1 | 2.3 KB |
  | `target.resolve` (one block id) | 0.51 ms | 1.8 | 2.5 | 0.4 KB |
  | `target.resolve` (8 ids) | 1.02 ms | 8.2 | 13.6 | 2.5 KB |
  | `com.superlogical.terminal.size` | 0.73 ms | 4.2 | 9.3 | 0.2 KB |
  | `session.describe_block` | 4.58 ms | 14.1 | 65.5 | 49.8 KB |

  Creating blocks is slower: `new_split` 29 ms and `new_layer` 59 ms for one
  block (single runs). The view grows with the number of blocks; a session
  with hundreds of blocks was not measured.
- **Cheapest existence check: `target.resolve`** with
  `{"kinds":["block"],"queries":["block:..."],"context":{"session_id":S}}`.
  It answers `{"results":[{"query":...,"status":"resolved","target":{"kind":"block","session_id","window_id","block_id","label","where":"window 1"}}]}`
  or `{"status":"not_found"}` (a missing block is a normal result, not an error),
  takes many queries in one call, needs no attach, and also finds detached
  blocks (docs: "Block searches include detached blocks"). A full block ID is
  only tried as a full ID, and only in `context.session_id`'s session. It does
  not say whether the block is in a given window or layer; for that use
  `session.view`. Failing calls also work as a check: a block method such as
  `com.superlogical.terminal.size`, `block.method` or `session.describe_block`
  on a missing block returns `{"code":"not_found","status":404}` (observed),
  at about the same cost except `describe_block`.

## R6. Closing the last block of a layer; closing a layer

**Yes to both.**

- **`block.close` on a layer's last block closes the layer.** Docs
  (`session.detach_block`, same rule for taking a block out): "If it was the
  only block in a floating layer, the layer closes." `session.move_block`
  docs: "If the move empties a floating layer, the server removes it."
  Observed: a window with a tiled block `t` and floating layer A (one block):
  `block.close` of A's block → `{"block_id":"block:…c038","revision":973}`, and
  the next view had only the tiled layer: `[["tiled",["t"]]]`. For a layer with
  two blocks B1, B2: after closing B1 the layer stayed (`["floating",["B2"]]`),
  after closing B2 it was gone. The events were only `block_closed` and
  `session_view_changed`; there is no layer-closed event.
- **`session.close_layer` closes every block in the layer.** Docs: "Close a
  floating layer and every block in it... The call returns once the blocks
  have closed." Observed: a layer with C1, C2: the response was
  `{"revision":980}`, the events were `block_closed` x2 and
  `session_view_changed`, neither block was in `list_blocks`, a method call on
  C1 gave `not_found`, and the process (pid 80942, C1's `login`) was gone.
  `close_layer` on the tiled layer fails `bad_request` (docs).
- **Focus and the window.** Docs: "If the layer had focus, focus returns to
  the layer that had it before, at the block last focused there. Otherwise
  focus doesn't change." and "If the layer held the window's last blocks, the
  server closes the window too." Observed: with the window's tiled block gone
  and one float left, the view had an empty tiled layer
  (`[["tiled",[]],["floating",["D"]]]`) with the float focused; `close_layer`
  on that float then closed the window: events `window_closed`,
  `active_window_changed`, `block_closed`, `session_view_changed`.
  `block.close` has a `keep_last` flag that fails rather than close a
  window's only block, floating layers counted (docs).
