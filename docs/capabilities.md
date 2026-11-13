# Pane provider capabilities

What a command pane provider's capability is given, what it prints, and what
its exit status means. Decided in #86 and #93, recorded in ADR 0006;
choosing a pane provider is ADR 0007. `<name>` and `<NAME>` stand for the
tool's name, lower and upper case. The code is `src/providers/command/`; a
test checks every capability and variable it runs with appears here.

This is protocol 1: a command gets it as `<NAME>_PROTOCOL`, the highest
number both its table's `protocols` and the daemon list. New variables,
optional capabilities and JSON fields leave the number as it is; changing
the meaning of an existing one bumps it. A command ignores variables it
doesn't know.

## Running a command

A capability is an argv, run once, without a shell; all but `events`,
which runs for as long as the pane provider is chosen (see [Events](#events)).

- **Working directory:** the choosing attach's, or `/` if it has gone.
- **stdin** is `/dev/null`. **stdout** is a pipe the daemon reads for the
  opens (`open`, `open_beside`, `open_float`, `open_tab_page`), `find`,
  `report_layout` and `events`, and `/dev/null` for every other
  capability, whose output is ignored.
  **stderr** is always a pipe the daemon reads. No other descriptor is
  inherited.
- **Process group:** its own. Whatever it starts is in that group, unless it
  makes a group of its own.
- **Environment:** the choosing attach's, less `<NAME>` and every variable
  in the table below, whether this capability gets it or not, and every
  `<NAME>_ENV_*`. The rest stays, `<NAME>_CONFIG`, `<NAME>_PROVIDER` and
  `<NAME>_TMPDIR` among them. Then the capability's own variables are set,
  and always `<NAME>_CAPABILITY`, `<NAME>_PROTOCOL` and `<NAME>_EXE`.
- **Timeout:** each capability has a default, which the table's `timeouts`
  changes, up to 60 s. At its timeout the command's group gets SIGTERM, and
  SIGKILL 1 s later; the act has failed, whatever it does after.
- **Its end** is watched with the kernel's process-exit notification
  (kqueue's `EVFILT_PROC` on macOS and the BSDs, a pidfd on Linux), not
  SIGCHLD: the daemon ignores the signals it watches, and an ignored
  SIGCHLD reaps children by itself, losing their exit statuses.
- The daemon never waits on a command: a slow one holds up nothing but the
  acts queued behind it. Layout acts (every capability but `focus` and
  `focus_direction`) run one at a time, in order. `focus` and
  `focus_direction` run one at a time, the newest waiting in place of any
  older one.

| Capability | Timeout |
|---|---|
| `open`, `open_beside`, `open_float`, `open_tab_page` | 5 s |
| `find` | 3 s, its own retries included |
| `close`, `place_beside`, `resize`, `focus`, `focus_direction` | 2 s |
| `report_layout` | 1 s |

`events` has no timeout: it runs for as long as the pane provider is
chosen (see [Events](#events)).

## Exit statuses

| Status | Meaning |
|---|---|
| 0 | done |
| 69 (`EX_UNAVAILABLE`) | unsupported: the daemon asks this capability no more, and does without it |
| anything else, or killed by a signal | failed: the daemon does without it this time; the first failure of each capability shows in Neovim with its first stderr line |

A command that can't start, such as a program not found, is unsupported.
`find`'s statuses mean more: see [`find`](#find).

The first 64 KiB of stderr of each run goes to the daemon's log, line by
line.

## Output

The opens and `find` print a terminal's id as the first line of stdout. It
is trimmed of ASCII whitespace, and is at most 4 KiB, with no NUL, in UTF-8:
anything else fails the act. An empty first line, or no output, is no id:
the terminal opened, but the daemon can't close, place or focus it. The
rest of stdout is ignored, as is all of every other capability's but
`report_layout`'s and `events`'.

### `find`

`find` tells the daemon whether a terminal that attached is one of the
pane provider's, and its id. It runs for a terminal that attached of its
own accord (a self-attach), and for one the pane provider opened when it
has no id for it: one whose open printed none, or, in a pair, every one
(see [Pairs](#pairs)). It runs in the choosing attach's environment, so
`$TMUX_PANE` there is the choosing attach's: the attaching terminal's own
variables reach it only as `<NAME>_ENV_<VAR>`, one for each of the table's
`find_vars` the terminal has, its value as raw bytes. It also gets the
attach's `PANE`, `PID`, `TTY` and `TITLE`.

Before the first `find` for an attach, the daemon writes the **pane
title**, `<name>-` and the token of the pane id (`TITLE`), into the
terminal's title, as ordinary output: it saves the terminal's own title
first (`CSI 22;2 t`), then writes the title with `OSC 2`. When the pane
hangs up or detaches, the title is cleared (`OSC 2` with nothing), then the
saved one put back (`CSI 23;2 t`); a terminal that keeps no title stack,
such as Ghostty or WezTerm, is left with no title. `find` matches a window
whose title contains `TITLE`, retrying for a while, since a window manager
may not list a new window at once. An open gets the same title as `TITLE`,
so a terminal provider can make it the window's first title rather than
rely on the terminal honouring the escape code. Rex's built-in `find` needs
no title, so a Rex pane never gets one; nor does a terminal with no pane
provider.

What each answer means depends on whose terminal it is:

| Answer | (a) the choosing attach | (b) a later self-attach | (c) a terminal the pane provider opened |
|---|---|---|---|
| exit 0, a line | accepted; that id | in reach; that id | that id |
| exit 0, nothing | accepted; no id | in reach; no id | no id |
| exit 69, or can't start | accepted; no id; `find` isn't run again until the next choice | `find` isn't run until the next choice: reach rests on context variables alone; no id | no id |
| anything else, a signal, a timeout | refused: no pane provider, with a warning | out of reach | no id, still in reach |

"No id" means every act that needs one does without: new windows split
off the terminal's pane open unplaced, it is never focused, and its pane is
a layout area of its own.

### Reach

A terminal is in the pane provider's reach if the pane provider opened it,
it is the choosing attach's, or it passes the pane provider's checks. A
window opened from a pane out of reach is drawn in that pane, not in a
terminal of its own.

- **Context variables** (`context_vars`) come first: each must have the
  value it had in the choosing attach, compared as bytes, unset matching
  unset. A terminal that differs is out of reach, and `find` doesn't run.
- Then **`find`**, as its table above says.
- With neither, every terminal is in reach.

### Pairs

A pair, `{ terminal = T, layout = L }`, runs only `T`'s `open` and `close`,
and `L`'s `find`, `place_beside`, `resize`, `focus`, `focus_direction`,
`report_layout` and `events`, with `L`'s `context_vars` and `find_vars`.
The one-step capabilities (`open_beside`, `open_float`, `open_tab_page`)
come only from a table returned whole, so a pair opens, then places, and has
no tab page areas.

The id `T`'s `open` prints serves only `close`; with none, `close` doesn't
run. `L`'s `find` runs for every attach, even when `open` printed an id,
and the id it prints serves everything else.

### `report_layout`

`report_layout` prints the live layout as one JSON object, at most 1 MiB:
anything longer, or malformed, fails the act, and the daemon keeps the
last layout it read.

```json
{
  "areas": [
    {
      "id": "@1",
      "terminals": [
        {"terminal": "%1", "rect": [0, 0, 0.5, 1]},
        {"terminal": "%2", "rect": [0.5, 0, 0.5, 1]},
        {"terminal": "%3", "rect": [0.25, 0.25, 0.5, 0.5], "floating": true, "inset": [1, 1, 1, 1]}
      ]
    }
  ],
  "focus": "%2"
}
```

- `areas`: every layout area, such as a window or a tab of the
  multiplexer, with an `id` of the pane provider's own (a string) and its
  `terminals`. Neovim's screen is the size of the current pane's layout
  area, and each area has its own tabline panes.
- `terminal`: the id an open printed. Foreign panes, terminals that aren't
  Neovim panes, belong too: they take room in their area. A terminal
  hidden while another is zoomed or full screen goes in an area of its own
  id, not the zoomed one's.
- `rect`: `[x, y, w, h]`, as fractions of the area, from its top left:
  what the terminal takes of its area, including any cells named in
  `inset`. Rects need not tile: cells between terminals, such as tmux's
  separators, may belong to none.
- `floating` (default `false`): it floats over the area's tiled terminals.
- `inset` (default `[0, 0, 0, 0]`): cells at each edge of `rect` that aren't
  the terminal's, such as a pane's frame: left, top, right, bottom. A
  tiled terminal's inset counts toward its area's size: the area is the
  terminal's cells plus its inset, divided by its share of the area, on
  each axis. The terminal's content starts inside it, at the rect's top
  left moved by the inset's left and top: `open_float`'s `X`,`Y` count
  from there.
- `focus` (optional): the terminal the user types into. Without `events`,
  a change here is how the daemon learns the user moved focus. A report
  whose command started before a `focus` or `focus_direction` command
  ended may have read focus before that act landed: its `focus` is
  ignored, and the report the daemon runs after the act tells it.
- Unknown fields are ignored.

Without `events`, the daemon runs `report_layout` after each of its own
commands has ended, whether it worked or not (one past its timeout once it
has been killed), and after a terminal attaches, resizes, hangs up or
reports focus; never on a timer. With `events`, it runs it when `events`
says, and at each start and restart of `events`. Either way it is a layout act, run one at a time with the others, and
at most one waits. Exit 69 makes every pane its own layout area.

### Events

`events` is long-running: it starts when the pane provider is chosen, and
is killed, with its process group, when the daemon drops the pane provider
or ends. Each line it prints, up to 64 KiB (longer lines are dropped), has
the daemon run `report_layout` again. A line that is a JSON object may also
say:

```json
{"focus": "%2"}
{"closed": "%3"}
```

- `focus`: the user moved focus to this terminal.
- `closed`: this terminal closed; only logged for now.

Any other line, such as an empty one, only has the layout read again.

A line's `focus` may have been read before a `focus` or `focus_direction`
command landed, and still come after it ended: followed as it came, it
would undo the daemon's own focus. So with `report_layout`, the daemon
doesn't follow it as it comes, but as the first report started after the
line, and after the last such command ended, says: that report's `focus`,
or, if it gives none, fails or exits 69, the line's own. A report that
started before a focus command ended tells nothing for it; a later line
does. Without `report_layout`, the daemon follows a line's `focus` as it
comes, so a line read before one of its focus commands landed can undo it.

When `events` ends, it starts again after 1 s, then 2 s, doubling up to
60 s; once it has run for 60 s, the wait is 1 s again. A restart that can't
start waits the same way. Exit 69, or a command that can't start the first
time, ends it for good: the daemon then runs `report_layout` as it would
without `events`.

## Built-in commands

A built-in pane provider other than Rex, such as tmux's, is a table whose
every capability runs `<name>` itself (ADR 0016):

```lua
find = { "{exe}", "provider", "builtin", "tmux", "find" },
```

- **Words:** `provider builtin NAME CAPABILITY`, hidden from help. The
  capability word wins over `<NAME>_CAPABILITY`.
- **Inputs:** only the variables this page documents for the capability,
  and the choosing attach's environment, such as `TMUX`.
- **Outputs:** the capability's stdout and exit status, as this page says.
  An unknown name, capability or extra word exits 69, unsupported, as does
  a capability the installed multiplexer is too old for.
- **Run by hand**, without `<NAME>_PROTOCOL`, it exits 2, saying a pane
  provider table runs it.

The words, inputs and outputs are frozen, and only ever gain forms: a
daemon upgraded in place runs a newer binary at `{exe}`. The commands it
runs, such as `tmux`'s, and its stderr are not.

### tmux

`<name>.builtin.tmux()`: every capability but `open`, on the tmux server
`TMUX` names. Every `tmux` call passes that socket with `-S`, and names the
pane or window it acts on with `-t`. A tmux window is a tab page area.

- `find`: the pane of the server whose tty is `TTY`, compared by device,
  as tmux itself tells a client inside it; `%N`, else exit 1. No context
  or find variables: `TMUX` leaks into terminals tmux doesn't run. It
  refuses tmux older than 3.2, naming the version, and a terminal of no
  pane with `this terminal (TTY) isn't a pane of the tmux server at
  SOCKET`. `<name>.inside.tmux` runs the same check.
- `open_beside`: `split-window -d`, `-h` or `-v` and `-b` from
  `DIRECTION`, `-l SIZE`, `-t ORIGIN`, running the attach command; prints
  the new pane's id.
- `open_float`: `new-pane -d -t ORIGIN` with `-x -y -X -Y`, running the
  attach command, its content on the float group: `X`,`Y` count from the
  top left of `ORIGIN`'s content, inside its inset, which is its
  `pane_left` and `pane_top`, and are made the window's from there. 3.7's flags give
  the content, a size under the window's; 3.8's give the frame, so it
  passes `X-1`, `Y-1`, `COLS+2`, `ROWS+2`. The frame stays: a float without
  one merges into the pane beneath. Prints the new pane's id; below 3.7,
  as `tmux -V` says, it exits 69.
- `open_tab_page`: `new-window -d -a` after `ORIGIN`'s window, running
  the attach command; prints the new pane's id. The session's current
  window stays: the daemon focuses the pane when Neovim goes to its tab
  page.
- `close`: `kill-pane`; done when the pane is already gone.
- `place_beside`: `join-pane -d`, `-h` or `-v` and `-b` from `DIRECTION`,
  `-l SIZE`, the terminal beside `ORIGIN`, without focusing it; then the
  `resize` below, since within one window the room the terminal leaves
  goes to its neighbour.
- `resize`: from `#{window_layout}`, read in tmux 3.8's JSON or the older
  string, the deepest split holding the terminal and `ORIGIN`; the
  terminal's part of it gets `SIZE` cells, by the border toward `ORIGIN`.
  One `resize-pane -x` or `-y` moves that border, on a pane of the part
  before it with no split of the same kind of its own, else on one of the
  part after it, when that part is the split's last. Where neither exists,
  as for `((P | A) / (X | Y)) | C | D`'s border before `C`, it exits 1.
  Under a top or bottom `pane-border-status`, `-y` on a pane by the status
  line sizes that pane's content, not its cell: when the pane resized is
  the other side's, the terminal's content comes a row short of `SIZE`,
  within the one cell.
- `focus`: `select-window` and `select-pane` on the terminal, in one tmux
  call, so every client on its session moves; never `switch-client`,
  which moves the most recently active client, whatever its session.
- `focus_direction`: `COUNT` steps in the window the most recently active
  client shows, else tmux's own current window, each a `select-pane -L`,
  `-R`, `-U` or `-D` only if the active pane isn't at that edge
  (`#{pane_at_left}` and the like), in one tmux call: tmux's own wraps
  round, this stops at the edge.
- `report_layout`: an area per window of the server, `@N`, a window
  linked into several sessions counted once, foreign panes included. Rects
  are each pane's content, as fractions of its window, with no inset; with
  `pane-border-status top`, a pane on the top row is reported from the top,
  one row taller, with an inset of 1 at the top. `focus` is the active pane
  of the window the most recently active client shows. A float is its
  frame, its content grown by a cell on each side, `"floating": true` with
  an inset of 1 all round; either may be partly off its window. In a
  zoomed window, `@N` holds only the panes it shows, the zoomed one at
  `[0, 0, 1, 1]`; the panes the zoom hides are in `@N~hidden`, where the
  unzoomed `#{window_layout}` has them, as their own `pane_*` are stale.
- `events`: one `tmux wait-for -E EVENT` waiter per event:
  `window-layout-changed`, `window-pane-changed`, `session-window-changed`,
  `client-session-changed`, `window-linked`, `window-unlinked`,
  `client-attached`, `client-detached` and `client-active`. A waiter is no
  client: `session_attached`, `list-clients` and the user's hooks don't see
  it, and panes still get focus-out. Its first line, once the waiters have
  had 30 ms to register, is where focus starts, `{"focus":"%N"}`, if a
  client shows any. A waiter that wakes is armed again; once no waiter has
  woken for 30 ms since the last was armed again (at most 250 ms), it
  prints `{"focus":"%N"}` if the focused pane, read as `report_layout`
  reads it, changed, else an empty line. A waiter failing
  within a second of starting is dropped; once every one is gone, it exits
  1. Below 3.8, as `tmux -V` says, it exits 69: the layout is then read
  after each act, and a focus move made with tmux's own keys is learned at
  the next key typed or focus report. Rejected: control mode (`tmux -C`),
  whose client counts as attached, runs the user's `client-attached` hooks
  and keeps every pane of its session from losing focus; and global
  hooks, which outlive a daemon killed.

`open_beside`, `place_beside`, `resize`, `close`, and `focus` on a hidden
pane unzoom its window, as tmux's own commands do; `open_tab_page` leaves
a zoom as it is.

## Placeholders

Every variable but `<NAME>_ENV_*` is also a placeholder: a whole argv word,
the lower-cased rest of its name in braces. `{terminal}` is
`<NAME>_TERMINAL`.

- Every capability gets `{capability}`, `{protocol}` and `{exe}`.
- `{attach}` alone becomes several words: the attach command's.
- A value the act doesn't have becomes one empty word.
- `{{name}}` passes `{name}`.
- A placeholder inside a longer word passes as written, with a warning when
  the config is checked. Other braces, such as tmux's `#{pane_id}`, are
  never looked at.
- A whole-word placeholder the capability isn't given makes the config
  check ignore that capability, with a warning.

## Capabilities

| Capability | Inputs | What it does |
|---|---|---|
| `open` | `attach` `attach_json` `title` `pane` `cols` `rows` | opens a terminal running the attach command, wherever the pane provider puts one, and prints its id |
| `open_beside` | `attach` `attach_json` `title` `pane` `origin` `direction` `size` `ratio` `cols` `rows` | opens it beside `origin`, on side `direction`, and prints its id |
| `open_float` | `attach` `attach_json` `title` `pane` `origin` `x` `y` `cols` `rows` `origin_cols` `origin_rows` `area_bounds` | opens it floating over `origin`, at `x`,`y` and `cols` x `rows`, and prints its id |
| `open_tab_page` | `attach` `attach_json` `title` `pane` `origin` `cols` `rows` | opens it in a new tab page area, and prints its id |
| `close` | `terminal` | closes the terminal; must succeed on one already gone |
| `find` | `pane` `pid` `tty` `title`, and `<NAME>_ENV_<VAR>` for each of `find_vars` | prints the attaching terminal's id (see [`find`](#find)) |
| `place_beside` | `terminal` `origin` `direction` `size` `ratio` | moves the terminal beside `origin`, on side `direction` |
| `resize` | `terminal` `origin` `direction` `size` `ratio` | sets the terminal's share of the split it makes with `origin`, the terminal across it |
| `focus` | `terminal` | makes the terminal the one the user types into, wherever it is |
| `focus_direction` | `direction` `count` | moves focus `count` steps in `direction` |
| `report_layout` | | prints the live layout (see [`report_layout`](#report_layout)) |
| `events` | | prints a line whenever the layout or focus changes (see [Events](#events)) |

Without `open_beside`, the daemon runs `open`, then `place_beside` once the
terminal has printed its id and attached.

## Variables

Each is `<NAME>_` and the name below.

| Variable | Format | Meaning |
|---|---|---|
| `CAPABILITY` | e.g. `open_beside` | the running capability |
| `PROTOCOL` | integer | the table's agreed number |
| `EXE` | path | the absolute path of the daemon's own binary, symlinks resolved, as it was when the daemon started: what a [built-in command](#built-in-commands) runs. Where it can't be found, a command naming `{exe}` can't start |
| `ATTACH` | one string | the attach words joined by spaces, each in single quotes unless it holds only `[A-Za-z0-9_@%+=:,./-]`; `eval "set -- $<NAME>_ATTACH"` gets them back |
| `ATTACH_JSON` | compact JSON array | the same words |
| `TITLE` | e.g. `<name>-k3j9x2qpa8` | the pane title |
| `PANE` | e.g. `12-k3j9x2qpa8` | the pane id the attach command names |
| `TERMINAL` | as an open or `find` printed it | the terminal acted on |
| `ORIGIN` | terminal id | the origin pane's terminal (for `open_float`, the current pane's); for `resize`, the terminal across the split |
| `DIRECTION` | `left`, `right`, `up`, `down` | a split: which side of `ORIGIN` the terminal goes or is on; `focus_direction`: where focus moves |
| `SIZE` | integer, cells | the terminal's size along the split |
| `RATIO` | e.g. `0.5`, `0.3333` | the terminal's share of the room it splits with `ORIGIN`: at most 4 decimals, a `.`, no exponent |
| `COLS`, `ROWS` | integer, cells | `open`, `open_beside`: a guess at the new terminal's size, its share beside its origin pane; `open_tab_page`: the origin's layout area; `open_float`: the float group's size |
| `X`, `Y` | integer, may be negative | the float group's top-left, in cells from the top-left of `ORIGIN`'s content: its `rect`'s top-left, as `report_layout` gives it, moved right and down by its `inset`'s left and top |
| `ORIGIN_COLS`, `ORIGIN_ROWS` | integer, cells | `ORIGIN`'s size |
| `AREA_BOUNDS` | `x,y,w,h`, each as `RATIO` | the float as fractions of `ORIGIN`'s layout area; unset outside a live layout |
| `COUNT` | integer, 1–20 | `focus_direction`'s steps |
| `PID` | integer | the attaching helper's process id |
| `TTY` | path | the attaching terminal's tty; empty where it has none |
| `ENV_<VAR>` | raw bytes | the attaching terminal's value of `VAR`, one of `find_vars`; unset where it has none. A variable only, never a placeholder |
