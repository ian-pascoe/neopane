# What neorex asks of Rex today

Catalog of every request the daemon sends to Rex and every event or view field
it reads, gathered for the pane-provider interface (map #76, ticket #77). All
citations are file:line in this repo at origin/main 066c0da. Rex's side is not
read here: only what neorex's own code sends and consumes. Wire details of
Rex's protocol (`rex.control.v1`, API revision 7, `src/rex/client.rs:15-16`)
are as neorex's client has them; whether Rex itself offers more is unconfirmed.

## How the daemon reaches Rex

- Startup needs `REX_SERVER` and `REX_SESSION` or the launcher refuses with
  `NotInRex` (`src/daemon/launch.rs:36-37`, `serve.rs:161-162`). The
  launcher's own pane block is `REX_BLOCK`, optional (`serve.rs:290-302`).
- One long-lived WebSocket (`rex.control.v1`) to `REX_SERVER`, polled in the
  event loop (`serve.rs:340-357`). Requests are matched to responses by id
  (`session.rs:512-516`, `take_pending_request`; `on_response` `:445-470`).
- Any Rex error or hang-up sets `Conn::Closed`; nothing more is sent and
  windows keep working without Rex (`Conn` doc, `session.rs:54`; `close_rex`, `session.rs:222-225`). Pending splits and
  layers then fail (`fail_pending_split`, `session.rs:500-510`).
- With Rex closed, `new_pane` does nothing: the window stays in Neovim
  (`daemon.rs:599-601`), no floating panes are made (`floating_panes.rs:62-66`),
  and `place_tabs` is skipped (`daemon.rs:999-1001`).
- Not a Rex request, but how every pane gets its terminal: the block runs
  `[exe, "pane", sock, id]` and that helper connects back to the daemon, which
  matches the hello to a pane it created and refuses any other
  (`daemon.rs:419-426`, `642-650`). This is the "accept only attaches it asked
  Rex for" behaviour the map describes.

## Requests

Each: Rex method, what the daemon uses it for, what breaks without it, and the
capability in Rex-neutral words. "Per-pane" vs "shared window" flags the
ticket's distinction.

- **attach** (`session.attach`, `session.rs:203-209`). Joins the session so
  events arrive; on success fetches a view (`on_attach_result`, `session.rs:518-528`). Called once
  from `Daemon::start` (`daemon.rs:246`). Refusal closes Rex (`session.rs:524`). Without it: no events, no
  layout, no focus. Capability: *subscribe to the pane provider's layout and
  focus changes for this session* (optional if the provider has no events).
- **new_split** (`session.new_split`, `daemon.rs:657-667`). Args: anchor
  block, direction (horizontal|vertical), side (before|after), and the new
  block's spec: a terminal running `neorex pane SOCK ID` with no shell and an
  initial size guess (`daemon.rs:642-650`). Answer: the new block id
  (`session.rs:671-710`). Used for every new window pane (an external Neovim
  window). Without it: no pane for a split; the window stays in Neovim
  (`daemon.rs:599-601`). A refusal marks the pane `RequestFailed` and windows
  that waited retry (`daemon.rs:1129-1137`). Capability: *make a pane split
  from pane X in direction D (before/after), running command C, and learn its
  id.* Per-pane (anchor-relative), though the new pane's size is "half the
  anchor" (`geometry.rs:35-58`), a guess until the pane reports its real size.
- **new_layer** (`session.new_layer`, `session.rs:397-421`). Args: optional
  window id (see "shared window" below), bounds as window fractions [x,y,w,h],
  a layout holding one block running the same pane command. Answer: block id
  and layer id (`session.rs:687-702`). Two uses:
  - a floating pane for a group of Neovim floats, bounds from the floats'
    screen position (`floating_panes.rs:95-112`, `layer_bounds`
    `:160-174`);
  - a tab page's first window, at fixed bounds [0.05,0.05,0.9,0.9]
    (`geometry.rs:17`, `daemon.rs:651-655`). Later windows of that tab page
    split it (`daemon.rs:605-616`, `1178-1186`).
  Without it: no floating panes (floats stay drawn inside their window's pane),
  and tab pages' windows fall back to splitting/not getting a pane. Capability:
  *make a floating pane over the pane area at relative bounds, running command
  C, and learn its id* — **optional**, Rex-only today. Shared-window: bounds are
  fractions of the Rex window.
- **focus_block** (`session.focus_block`, `session.rs:259-296`). Focus a pane
  by id. Used when Neovim's current window or float changes
  (`daemon.rs:860`, `886`), when input into an unfocused pane takes focus
  (`daemon.rs:578-579`), and after a pane is made if its window is current
  (`daemon.rs:1158-1160`, `1180-1182`, `1197`). Deduplicated against the last
  known focus (`session.rs:266`, `has_focused`). Without it: Neovim and Rex
  focus disagree (keys go to the wrong pane). Capability: *focus pane X.*
  Per-pane. A one-shot is plausible; the daemon sends it often and tracks
  in-flight requests to ignore echoes (`FocusedBlock::Requesting`,
  `session.rs:87-94`, `apply_focus` `:626-668`).
- **focus_direction** (`session.focus_direction`, `session.rs:299-310`).
  `<C-w>h/j/k/l` with count (≤20, `daemon.rs:153`) moves Rex focus to the
  nearest pane that way, "wrap": false (`daemon.rs:866-877`). The new focus
  comes back via the view as a user focus move. Without it: `<C-w>hjkl` cannot
  leave the Neovim windows' neighbours (e.g. cannot reach foreign panes).
  Capability: *focus the nearest pane left/right/up/down of the focused one,
  no wrap.* Relative to the focused pane (shared window's geometry, the
  provider's, since neorex can't compute adjacency to foreign panes).
- **view** (`session.view`, `fetch_view`, `session.rs:578-596`). Fetch the full session
  snapshot; sent after attach, after every `session_view_changed` event with a
  newer revision, and after a refused focus (`session.rs:472-483`,
  `on_focus_block_result` `:530-560`). See "What the daemon reads" below.
  Without it: no pane rects, no user-focus tracking, no moved-layer tracking.
  Capability: *report where each pane is and which is focused, and notify on
  change* — an optional live-layout/focus capability.
- **disown_block** (`session.disown_block`, `session.rs:324-330`). Called right
  after each made split or layer (`daemon.rs:1157`, `1177`). Per its comment:
  "The creator owns a block and sizes it; this lets the app size it instead"
  (`session.rs:323`). Without it: Rex would keep the size neorex asked for
  rather than layout-sizing the pane. Rex-specific ownership semantic;
  Rex-neutral capability: none (it's a Rex quirk of the new-pane request, not a
  need). It follows no feature of neorex's.
- **close_block** (`block.close`, `session.rs:333-340`). Closes a pane neorex
  made whose helper has not connected yet, when its window closes first
  (`daemon.rs:934-939`), or whose window went while Rex was creating it
  (`daemon.rs:1151-1154`, `1170`). For panes whose helper is connected,
  hanging up the terminal is what closes the Rex pane (`daemon.rs:934`
  comment). Without it: orphan empty panes in the narrow race. Capability:
  *close pane X*; only needed because neorex asks the provider to create a pane
  it does not yet hold.
- **close_layer** (`session.close_layer`, `session.rs:387-393`). Closes a
  floating pane when its float group goes away before its helper connected
  (`floating_panes.rs:78-81`), or whose group closed while Rex was making it
  (`daemon.rs:1169`). Capability: *close floating pane X* (optional, with
  floats). Same race-only role as close_block.
- **move_block** (`session.move_block`, `session.rs:343-364`). Moves a pane to
  sit right of (or below) an anchor pane, leaving the anchor a ratio of the
  shared room (`daemon.rs:1021-1034`). Used by `place_tabs` to rearrange a tab
  page's panes into Neovim's layout, because Rex has them in creation order
  (`daemon.rs:994-997`; `tab_placement.rs:1-9`). Without it: tab-page window
  panes sit in creation order, not Neovim's layout (e.g. after Diffview).
  Capability: *move pane X to beside pane Y in direction D with ratio R.*
  Optional (needs provider layout control).
- **resize_pane** (`session.resize_pane`, `session.rs:366-385`). Moves the
  border between two adjacent panes to a ratio (`daemon.rs:1035-1048`), used
  when a layout's windows and order are unchanged but sizes changed
  (`tab_placement.rs:8-9`). Without it: pane sizes drift from Neovim's window
  sizes. Capability: *set the split ratio between adjacent panes X and Y.*
  Optional.

## What the daemon reads

Events: only `session_view_changed` is acted on, via its `revision` int, to
trigger a `view` fetch (`on_event`, `session.rs:472-483`). Every other event is ignored
(`session.rs:473`). Responses read: `block_ids[0]` of a split or layer, and
`layer_id` of a layer (`session.rs:671-710`); a refusal is "failed".

View fields (`apply_view`, `session.rs:600-624`, `src/daemon/rex_layout.rs`):

- `revision`: skip views no newer than the last (`session.rs:601-604`).
- `active_window_id` + each window's `window_id` and `focused_block_id`: the
  focused block of the active window (`find_focused_block`,
  `session.rs:712-728`). A change not caused by neorex is the **user focusing
  a pane in Rex** → `FocusMoved` → neorex makes that pane's Neovim window
  current (`daemon.rs:1203-1235`). Focus echoes of our own request are
  filtered (`session.rs:626-668`). Without it: clicking or `focus_direction`
  in Rex doesn't change Neovim's current window. Capability: *tell neorex
  which pane has focus, when it changes.* Per-pane.
- `windows[].layers[]` with `kind` ("tiled" or other = floating), `layout`
  trees holding `{block_id, rect:[x,y,w,h]}` and floating layers' `bounds` and
  `blocks[0].block_id` (`rex_layout.rs:35-117`). Rects and bounds are
  **fractions of the window**.
- `windows[].window_id` (43 chars) of the window holding our tiled panes, kept
  as `rex.window` and sent back as `window_id` in `new_layer`
  (`daemon.rs:1269-1281`, `session.rs:405-409`). Without it a new layer would
  have no window to go in (the field is optional on the wire; effect of
  omitting it unconfirmed).

Uses of the rects and layer bounds:

- **Window size for Neovim's screen** (shared window): the editor spans the
  whole Rex window, computed as the pane's size divided by its rect share
  (`nvim_session.rs:262-278`, `fit_ui_to` `:237-249`). Without rects it falls
  back to the pane's own size.
- **Tabline panes** (shared window): panes whose rect origin is the window's
  top row show their share of Neovim's tabline, left to right by rect x
  (`geometry.rs:99-176`; the tabline width is the sum of their columns,
  `:124-139`). Without rects: a pane with no rect counts only while none has
  one (`geometry.rs:118-120`), so a single pane is the whole window.
- **Pane origin** (`geometry.rs:73-85`): rect × screen size, used for the
  tabline start column and in floating-pane placement.
- **Floating-pane follow** (shared window): if the user moved/resized a floating
  pane, its `bounds` differ from the daemon's by more than half a cell
  (`floating_panes.rs:176-191`) and the Neovim floats are re-placed to follow
  (`floating_panes.rs:40-56`, `daemon.rs:1282-1284`). Floating row/col are
  also computed from bounds × grid 1 size (`floating_panes.rs:118-155`).
- **Layout settling** (`awaiting_layout`, `daemon.rs:443-450`, `462-475`):
  a new split's terminal is created at the guessed size, so a hello at the
  default 80x24 means "Rex hasn't laid it out yet"; the daemon waits ≤1s
  (`LAYOUT_WAIT_MS`, `daemon.rs:148`) for a resize before trusting it.

## Facts that don't come from Rex requests

- Per-pane facts neorex already gets by attach, no Rex needed: the terminal fd
  (SCM_RIGHTS, via the pane helper), its size (`TIOCGWINSZ`,
  `daemon.rs:539-550`), resizes (the helper's `Resized` byte,
  `daemon.rs:530-533`), terminal focus in/out reports
  (`forwarder.focused`, `daemon.rs:392-413`), input, and a pane going away
  (EOF on its fd → `pane_gone`, `daemon.rs:700-720`, which closes its Neovim
  window or stops the daemon when it was the last pane).
- Reading `REX_BLOCK`/`REX_SESSION`/`REX_SERVER` from the environment is the
  only way neorex learns which Rex block it started in and which session to
  use (`serve.rs:161-162`, `290-299`).

## Shared-window assumptions vs per-pane

Assume one shared window (fractions of it, one coordinate space):

- Pane `rect` and floating `bounds` as fractions (`rex_layout.rs:35-117`).
- Editor size = pane size / rect share (`nvim_session.rs:262-278`).
- Tabline panes along the top and the tabline width (`geometry.rs:99-176`).
- Floating pane bounds, the fixed tab-page bounds, and following a moved layer
  (`geometry.rs:17`, `floating_panes.rs:40-56`, `158-191`).
- Pane adjacency for `focus_direction`, and the `move_block`/`resize_pane`
  ratios (`tab_placement.rs`).
- `new_pane_size` guess: half the anchor's terminal (`geometry.rs:35-58`).

Per-pane (no shared window needed): new split from an anchor, focus a pane,
close a pane, pane focus changes, the pane's own size, input and exit.

## What this means for the pane provider interface

- **The minimum core ask is small.** Without any provider, the daemon needs
  nothing from Rex: it already does everything per-pane (terminal, size,
  resize, focus reports, exit) through attach. New external Neovim windows
  simply have no pane (`daemon.rs:599-601`, the same path as a closed Rex).
- **Required capability candidates (one-shot expressible):**
  1. *Create a pane* running command C, split from pane X in direction D and
     side (before/after), returning the new pane's id. Maps to new_split. The
     id comes back, so a user-written provider must print it, or neorex must
     otherwise learn it (the helper's hello carries only neorex's own pane
     number, `daemon.rs:422`, not the provider's id; so the provider-side
     id is needed only for the optional operations below).
  2. *Focus pane X* (focus_block). One-shot, required for `<C-w>` to be
     coherent; could be optional (focus then follows Neovim only one way).
  3. *Close pane X* (close_block), only for the race where a window closes
     before its helper connected. A provider that closes the pane when its
     command exits makes this unnecessary.
- **Optional capabilities:**
  - Focus nearest pane in direction (focus_direction): one-shot, but needs the
    provider to know adjacency including foreign panes.
  - Live layout + focus events (attach/view/`session_view_changed`): needs a
    long-running listener; it gives rects, user focus changes and moved
    layers. Everything shared-window (editor size, tabline panes, floating
    follow) depends on it; without it the daemon treats the pane as the whole
    window (`nvim_session.rs:268-270`, `geometry.rs:118-120`).
  - Floating panes (new_layer, close_layer) and floating tab pages.
  - Rearrange/resize (move_block, resize_pane) for placing tab-page panes.
- **Not an interface item:** disown_block (a Rex ownership quirk) and the
  initial-size guess in the new-pane request (neorex can only hint it; the
  terminal reports its real size by attach).
- **Pane identity:** the provider's pane/block id is used throughout
  (`Pane.rex_block`, `pane.rs:26`), and matched with ids from the view.
  Rex's ids are fixed-width strings (`rex/ids.rs`: block 42 bytes, window 43);
  a provider-neutral id should be an opaque string.
- **Shared-window things need a provider-neutral answer in a later ticket:**
  how the tabline panes and editor size are found without the provider's
  fractional rects (e.g. from attach-side facts or optional provider geometry).
