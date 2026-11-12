# Every pane provider capability is an optional one-shot command

Amended by ADR 0007 before implementation: no capability is required, a
terminal provider may be paired with a layout provider, reach checks context
variables, and a terminal carries a pane title for `find` to match.

Amended by ADR 0008 before implementation: `report_layout` gives each
terminal's rect as fractions of its layout area.

Amended by ADR 0010 before implementation: without `open_float`, a float
group is drawn in a pane, and there is no two-step float. A layout-side
`place_float`, and `<NAME>_KIND` for `open`, are recorded there as the form
they take once a pane provider needs them.

Amended by ADR 0012 before implementation: `open_tab_page` gives a tab page
a tab page area, from a pane provider returned whole; without it a tab page
is drawn in the pane it opened in. A layout-side `place_tab_page`, and
`tab-page` for `<NAME>_KIND`, are recorded there alongside ADR 0010's
`place_float`.

Amended by ADR 0013 before implementation: `{attach}` is `<exe> attach
--address A --pane ID`, the pane title's token is the pane id's, and a late
terminal's helper exits quietly.

Amended by #92 before implementation: the daemon keeps both queues itself, for
Rex too, sending layout acts one at a time; `open_beside` and `open_tab_page`
get `<NAME>_COLS`/`_ROWS`, `open_float` gets `<NAME>_ORIGIN_COLS`/`_ROWS`;
`resize`'s inputs, the direction and count are named; `report_layout` may give
an `inset`; Rex's `find` checks the attaching terminal is `REX_BLOCK`'s.

Amended by #93 before implementation: capability names are written with `_`
everywhere; every variable a capability gets is also a whole-word placeholder,
the opens get `{pane}` and `<NAME>_ATTACH_JSON` and `open` gets
`<NAME>_COLS`/`_ROWS`; a capability holding a placeholder it isn't given is
ignored; commands run in the choosing attach's working directory, without the
variables the daemon sets; `find`'s answers are set out for each use.

Amended by #94 before implementation: under the relay diagnostic, `{attach}`
ends with `--transport relay`.

Amended by #126 (SPI-664), following #86 decision D7 and #93 decision 18:
`events` is the one capability that isn't one-shot. It is long-running: it
starts when the pane provider is chosen, and is killed with its process
group when the pane provider is dropped. When it ends it is started again
after 1 s, the wait doubling up to 60 s and back to 1 s once it has run for
60 s, and a restart that can't start waits the same way. Exit 69, or a
command that can't start at its first start, ends it until the pane provider
is chosen again. Each line it prints only says the layout changed: the daemon
asks `report_layout` through its layout queue, one at a time like every
layout act, at most one waiting.

A pane provider gives the daemon terminals for Neovim's windows, and may place
and focus them (#86). Each thing it can do when the daemon asks is a
capability, and each is one command run once, so a user can write a pane
provider, such as Ghostty with AeroSpace, in a few lines with no long-running
process. No capability is required: `open`, which opens a terminal running the
attach command it is given, is what makes a pane provider a terminal provider,
and one without it only places and focuses terminals attached by hand (#87).
When a capability is missing or fails, that one act falls back as if there
were no pane provider: the window is drawn in its origin pane, a tab page in
the pane it opened in, its terminal is left unplaced, or the next key
switches focus back.

## Considered Options

- **JSON on stdin.** Structured, but every shell one-liner would need `jq`.
- **`open` placing the terminal when given an origin, if it can.** The daemon
  couldn't tell whether it did, so it would place every terminal again.
- **A `capabilities` command or table.** It goes stale against the commands;
  smart-splits.nvim dropped its own.
- **`open` required.** A layout provider alone would be a mistake in config,
  yet a pane provider whose `open` turns out unsupported is in that state
  anyway, and placing and focusing terminals attached by hand is worth having.
- **`focus` required too.** It shuts out terminal providers with no window
  manager to ask, and without it the next key already brings focus back.
- **A new terminal for every window, whatever pane it came from.** For a pane
  in another context, such as another Rex session, it opens where nobody is
  looking.
- **Placeholders replaced inside words**, as in `--title={title}`. One more
  quoting rule for every value, when `sh -c` and the variable already do it
  for the cost of one process. - **Finding a window only by its terminal's
  process.** Many terminals run every window in one process, so a layout
  provider paired with one couldn't tell its windows apart; a title works with
  any terminal that shows it.

## Consequences

- The capabilities are `open` and `close`, the terminal provider's; `find`,
  `place_beside`, `resize`, `focus`, `focus_direction`, `report_layout` and
  `events`, the layout provider's; and `open_beside`, `open_float` and
  `open_tab_page`, which open and place in one step and come only from a
  pane provider returned whole, never from a pair's halves. Without
  `open_beside`, as always for a terminal provider paired with a layout
  provider, the daemon opens the terminal, then places it once it attaches.
  Without `open_float`, a float group is drawn in a pane (ADR 0010).
  `open_tab_page` is given `{attach}`, `{title}` and the origin pane's
  terminal, opens a terminal running the attach command in a new tab page
  area, and prints its terminal id; without it, a tab page is drawn in the
  pane it opened in (ADR 0012). `focus` makes a terminal the one the user
  types into wherever it is, switching tmux window, herdr tab or workspace as
  needed. A capability is added only when a pane provider needs it.
- A command is an argv run without a shell, in the choosing attach's working
  directory, or `/` if that has gone. Its inputs are `<NAME>_*` variables, and
  the same as whole-word placeholders: each variable but `<NAME>_ENV_*` by its
  name after `<NAME>_`, in lower case, so `{terminal}` is `<NAME>_TERMINAL`.
  `{attach}` alone becomes several words, the attach command's: `<exe> attach
  --address <address> --pane <pane id>`, `<exe>` being the daemon's own
  executable; a daemon started with `<NAME>_TRANSPORT=relay` adds `--transport
  relay` (#94). `<NAME>_ATTACH` is the same as one string any shell reads, for
  a pane provider that types it, and `<NAME>_ATTACH_JSON` as a JSON array, for
  one that speaks JSON over a socket. A placeholder is replaced only as a
  whole word, never inside one, so a value inside a word goes through `sh -c`
  and the variable, and `{{name}}` passes `{name}`. A capability whose argv
  holds a whole-word placeholder it isn't given is ignored, with a warning, as
  an unknown capability is. It gets stdio only, and the environment of its
  pane provider's choosing attach without `$<NAME>` and without any variable
  the daemon sets for a capability, so none leaks in from an earlier command.
- Exit 0 is done; stdout holds a terminal id, or JSON from `report_layout`,
  which gives each terminal's rect as fractions of its layout area, and may
  give an `inset`: the cells at each edge of the rect that aren't the
  terminal's own; stderr goes to the log. Exit 69, or a command that can't
  start, means unsupported until the pane provider is chosen again; any other
  failure, or a timeout, fails that act alone, and nothing is retried.
- A terminal id comes from `open`'s stdout or from `find`, which is given the
  attach's pid, tty, pane id, pane title and, as `<NAME>_ENV_<VAR>`, the
  variables the pane provider names: `find` runs in the choosing attach's
  environment, so the attaching terminal's own variables reach it only that
  way, and the daemon never knows a pane provider's own variable names. In a
  pair, the terminal provider's id from `open` serves only `close`, and the
  layout provider's `find` runs for every attach, even when `open` printed an
  id; its id serves the rest. Ids are opaque, and the daemon never acts on a
  pane whose attach has ended; a pane provider whose ids can be reused puts
  what proves them current into the id.
- `find` answers the same way for each use. Exit 0 with a line gives that
  terminal's id. Exit 0 with nothing gives none, and every act that needs it
  falls back. Exit 69, or a command that can't start, accepts the choosing
  attach, leaves reach to context variables alone and gives no ids until the
  pane provider is chosen again. Anything else, or a timeout, refuses the
  choice for the choosing attach, puts a later self-attached terminal out of
  reach, and leaves a requested terminal without an id, unplaced. Context
  variables are compared first; a terminal that fails them is out of reach and
  `find` doesn't run.
- Before the first `find` command runs for an attach, the daemon writes a pane
  title into the terminal's title: `<name>-` and the token of the pane's id
  (ADR 0013), sent as ordinary output, so it works over a relay too. `open`
  gets it as `{title}` and `<NAME>_TITLE`, so a terminal provider can make it
  the window's first title rather than rely on the terminal honouring the
  escape code. `find` gets it as `<NAME>_TITLE` and matches a window whose
  title contains it, retrying for a while, since a window manager may not list
  a new window at once. It has to last only until `find` answers. The
  terminal's own title is saved first, and put back, or cleared where it can't
  be, when the pane hangs up. Rex's in-process `find` doesn't need a title, so
  Rex panes never get one: it asks Rex for the process of the block
  `REX_BLOCK` names and accepts the attach only if that process's controlling
  terminal is the attach's own; where Rex or the system can't say, it trusts
  `REX_BLOCK`, as before.
- A terminal provider paired with a layout provider must open each terminal in
  a window of its own: a terminal in a split or a tab is found, focused and
  placed as its whole window.
- A terminal is in a pane provider's reach if the pane provider made it, it is
  the choosing attach's, or it passes every check the pane provider has: its
  context variables, which must have the values the choosing attach had, then
  `find`; with neither, every terminal is. Context variables name what the
  commands act within, such as a Rex session; tmux, whose commands reach every
  session of a server, has none and checks the terminal's tty in `find`. A
  pane provider whose variables leak into terminals it doesn't run needs a
  `find`. A window opened from a pane out of reach is drawn in that pane.
- A window opened without being entered gets a terminal only when the daemon
  can focus the current window's terminal again; otherwise it is drawn in its
  origin pane, as plain Neovim draws `:pedit`.
- `close` is called whenever the daemon is done with a requested terminal, so
  it must succeed on one already gone. A self-attached terminal is never
  closed. A pane's helper never exits within 300 ms of starting, so a terminal
  that treats a quick exit as a crash, as Ghostty does, still closes; one
  refused for arriving late exits with status 0 and no message, which closes
  a terminal whose `open` printed no id to `close` it by (ADR 0013).
- Each pane provider's acts go through two queues the daemon keeps itself, for
  a built-in pane provider as for commands. Layout acts (`open*`, `find`,
  `place_beside`, `resize`, `close`, `report_layout`) run one at a time, each
  sent only once the one before is answered or has timed out, since Rex
  applies some back-to-back layout requests out of order. Focus acts (`focus`,
  `focus_direction`) have one running and at most one waiting, the newest
  replacing it, so a slow `open` never holds up `<C-w>w`.
- The opens get `{attach}`, `{attach_json}`, `{title}` and `{pane}`. `open`,
  `open_beside` and `open_tab_page` get `<NAME>_COLS` and `<NAME>_ROWS`, a
  guess at the new terminal's size: for `open` and `open_beside`, its share
  beside its origin pane; for `open_tab_page`, the origin's layout area.
  `open_float` gets the float group's size in them, and `<NAME>_ORIGIN_COLS`
  and `<NAME>_ORIGIN_ROWS`, the reference terminal's. The terminal acted on is
  `<NAME>_TERMINAL`, and the origin pane's terminal, or for `resize` the one
  across the split, `<NAME>_ORIGIN`. `docs/capabilities.md` will list every
  capability's inputs and their formats. `<NAME>_RATIO` is always the share of
  the terminal being opened, placed or resized. `resize` takes `{terminal}`
  and `{origin}`, the terminals either side of the split, and sets
  `{terminal}`'s share with `<NAME>_SIZE` and `<NAME>_RATIO` as `place_beside`
  gives them. A split's direction and `focus_direction`'s are
  `<NAME>_DIRECTION` (`left`, `right`, `up`, `down`), and its count
  `<NAME>_COUNT`. None bumps `<NAME>_PROTOCOL`.
- New variables, optional capabilities and JSON fields leave `<NAME>_PROTOCOL`
  as it is; changing the meaning of an existing one bumps it. Each half of a
  pair agrees its own number. Commands ignore variables they don't know, and
  the daemon ignores unknown JSON fields and, with a warning, unknown
  capabilities, as an older running daemon reading a newer config must.
- Built-in pane providers, Rex's first, offer the same capabilities
  in-process. Rex's is only ever used whole, never as half of a pair.

Research: `docs/research/rex-capabilities.md`, `tmux-pane-provider.md`,
`herdr-pane-provider.md`, `aerospace-ghostty-pane-provider.md`,
`pane-provider-prior-art.md`, `pane-provider-capabilities.md`, `pane-title.md`
and `rex-provider-facts.md`, each on its `research/` branch.
