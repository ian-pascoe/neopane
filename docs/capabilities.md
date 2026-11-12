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
  opens (`open`, `open_beside`, `open_float`, `open_tab_page`),
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
  and always `<NAME>_CAPABILITY` and `<NAME>_PROTOCOL`.
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

The first 64 KiB of stderr of each run goes to the daemon's log, line by
line.

## Output

The opens print the new terminal's id as the first line of stdout. It is
trimmed of ASCII whitespace, and is at most 4 KiB, with no NUL, in UTF-8:
anything else fails the act. An empty first line, or no output, is no id:
the terminal opened, but the daemon can't close, place or focus it. The
rest of stdout is ignored, as is all of every other capability's but
`report_layout`'s and `events`'.

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
  Neovim panes, belong too: they take room in their area.
- `rect`: `[x, y, w, h]`, as fractions of the area, from its top left.
- `floating` (default `false`): it floats over the area's tiled terminals.
- `inset` (default `[0, 0, 0, 0]`): cells at each edge of `rect` that aren't
  the terminal's, such as a floating pane's frame: left, top, right,
  bottom.
- `focus` (optional): the terminal the user types into. Without `events`,
  a change here is how the daemon learns the user moved focus.
- Unknown fields are ignored.

Without `events`, the daemon runs `report_layout` after each of its own acts,
whether it worked or not, and after a terminal attaches, resizes, hangs up or
reports focus; never on a timer. With `events`, it runs it when `events`
says. Either way it is a layout act, run one at a time with the others, and
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

When `events` ends, it starts again after 1 s, then 2 s, doubling up to
60 s; once it has run for 60 s, the wait is 1 s again. A restart that can't
start waits the same way. Exit 69, or a command that can't start the first
time, ends it for good: the daemon then runs `report_layout` as it would
without `events`.

## Placeholders

Every variable but `<NAME>_ENV_*` is also a placeholder: a whole argv word,
the lower-cased rest of its name in braces. `{terminal}` is
`<NAME>_TERMINAL`.

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
| `place_beside` | `terminal` `origin` `direction` `size` `ratio` | moves the terminal beside `origin`, on side `direction` |
| `resize` | `terminal` `origin` `direction` `size` `ratio` | sets the terminal's share of the split it makes with `origin`, the terminal across it |
| `focus` | `terminal` | makes the terminal the one the user types into, wherever it is |
| `focus_direction` | `direction` `count` | moves focus `count` steps in `direction` |
| `report_layout` | | prints the live layout (see [`report_layout`](#report_layout)) |
| `events` | | prints a line whenever the layout or focus changes (see [Events](#events)) |

Without `open_beside`, the daemon runs `open`, then `place_beside` once the
terminal has printed its id and attached. `find` is not run
yet.

## Variables

Each is `<NAME>_` and the name below.

| Variable | Format | Meaning |
|---|---|---|
| `CAPABILITY` | e.g. `open_beside` | the running capability |
| `PROTOCOL` | integer | the table's agreed number |
| `ATTACH` | one string | the attach words joined by spaces, each in single quotes unless it holds only `[A-Za-z0-9_@%+=:,./-]`; `eval "set -- $<NAME>_ATTACH"` gets them back |
| `ATTACH_JSON` | compact JSON array | the same words |
| `TITLE` | e.g. `<name>-k3j9x2qpa8` | the pane title |
| `PANE` | e.g. `12-k3j9x2qpa8` | the pane id the attach command names |
| `TERMINAL` | as an open printed it | the terminal acted on |
| `ORIGIN` | terminal id | the origin pane's terminal (for `open_float`, the current pane's); for `resize`, the terminal across the split |
| `DIRECTION` | `left`, `right`, `up`, `down` | a split: which side of `ORIGIN` the terminal goes or is on; `focus_direction`: where focus moves |
| `SIZE` | integer, cells | the terminal's size along the split |
| `RATIO` | e.g. `0.5`, `0.3333` | the terminal's share of the room it splits with `ORIGIN`: at most 4 decimals, a `.`, no exponent |
| `COLS`, `ROWS` | integer, cells | `open`, `open_beside`: a guess at the new terminal's size, its share beside its origin pane; `open_tab_page`: the origin's layout area; `open_float`: the float group's size |
| `X`, `Y` | integer, may be negative | the float group's top-left, in cells from `ORIGIN`'s top-left |
| `ORIGIN_COLS`, `ORIGIN_ROWS` | integer, cells | `ORIGIN`'s size |
| `AREA_BOUNDS` | `x,y,w,h`, each as `RATIO` | the float as fractions of `ORIGIN`'s layout area; unset outside a live layout |
| `COUNT` | integer, 1–20 | `focus_direction`'s steps |

`PID`, `TTY` and `ENV_*` are `find`'s, and are stripped all the same.
