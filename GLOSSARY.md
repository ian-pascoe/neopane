# neopane

neopane runs Neovim in the background and shows its windows in the terminals attached to it, which a pane provider, such as Rex, can open and place.

## Language

### Neovim

**Neovim's screen**:
The area Neovim lays its editor out on, `'columns'` by `'lines'` cells, drawn as grid 1. It is the size of the current pane's layout area, in the current pane's cells, or of the current pane alone while a float group too big for it is current with no float group pane. Only its tabline, command line, messages and floats are shown.
_Avoid_: editor, editor size, default grid

**Screen float**:
A float placed on Neovim's screen (`relative='editor'`) rather than on a window. Screen floats are drawn in the current pane, except float groups, which have a float group pane or stay in the pane they were last current in.
_Avoid_: editor float

**Tab page**:
Neovim's tab page.
_Avoid_: tab (on its own)

**Built-in tabline**:
The tabline Neovim shows when `'tabline'` is empty: a label per tab page, then a close button.
_Avoid_: default tabline, Neovim's own tabline

### neopane

**Daemon**:
What a user starts, attaches terminals to, lists and kills: one Neovim, run in the background, showing its windows in the Neovim panes attached to it. A user may run several.
_Avoid_: session (Rex's, tmux's and `:mksession`'s), server (Neovim's `--listen`), instance, workspace

**Daemon name**:
What a daemon is found by: unique among a user's running daemons, and by default the name of the directory it was started in.
_Avoid_: session name

**Daemon address**:
Where a daemon takes attaches and control requests: a Unix socket path, or a named pipe on Windows, written `//./pipe/…`. Passed whole, never taken apart.
_Avoid_: socket, socket path

**Launcher**:
The `<name>` a user runs to start a daemon: it starts the daemon, then stays in its terminal as the helper of the daemon's first attach, or with `--detach` exits once Neovim is up.
_Avoid_: client, wrapper, starter

**Command word**:
A first argument `<name>` reads as a command, such as `list` or `attach`, never as a file to open; `./list` opens the file.
_Avoid_: verb

**Neovim arguments**:
What follows `<name>`'s own options when it starts a daemon, given to Neovim as typed. A running daemon takes only a line and one file.
_Avoid_: passthrough, extra args

**Helper**:
The process a Neovim pane's terminal runs, `<name> attach`: it connects the terminal to a daemon, owns the terminal's settings, and stays until the attach ends.
_Avoid_: client, shim, proxy, pane process

**Attach**:
One helper's connection to a daemon for one terminal, from its hello to its end: a requested pane's, which carries its pane id, or a self-attach.
_Avoid_: session, connection

**Hello**:
A helper's first message: its purpose, its transport, its terminal's size and where it comes from (environment, working directory, tty, pid). The daemon answers every one, accepting or refusing it with a reason.
_Avoid_: handshake (the hello and its answer together), greeting

**Transport**:
How an attach's terminal reaches the daemon: handed over, so the daemon reads and writes it itself (Unix only, and the default), or relayed, the helper copying its bytes both ways (everywhere, and the only one on Windows).
_Avoid_: mode (a terminal's), channel (Neovim's), link

**Attach protocol version**:
The number a helper and a daemon must share to attach. Status and stop work whatever it is.
_Avoid_: protocol (on its own), `<NAME>_PROTOCOL` (the pane provider interface's)

**Control request**:
A connection that asks a daemon something without a terminal: status; stop, which a daemon with unsaved changes refuses unless forced; or open FILE, which may wait until the file is done.
_Avoid_: command (a capability's), RPC (Neovim's)

**Waited file**:
A file `<name> FILE` opens from a daemon's own `:terminal`: `<name>` returns once no window shows it, or its buffer is unloaded or deleted, so `EDITOR=<name>` works there too; `--no-wait` returns once Neovim has its window, or fails after 10 s.
_Avoid_: remote-wait (nvr's)

**Unattached daemon**:
A daemon with no Neovim pane attached or on its way, whether it hasn't had one yet or its last pane went.
_Avoid_: detached daemon (see Kept daemon), headless, orphan, paneless

**Kept daemon**:
A daemon asked to stay, by starting it with `--detach` (later also by a `detach`): it outlives its last pane with nothing unsaved, until Neovim quits or it is killed.
_Avoid_: detached daemon, persistent daemon, server

**First attach**:
The attach that takes a daemon from unattached to attached: the one `neopane` makes when it starts a daemon, the first to a daemon started with `--detach`, or the first after its last pane went.
_Avoid_: initial attach

**Unsaved changes**:
A buffer `:qa` would refuse to quit over: modified and writable. A running `:terminal` job is never one.
_Avoid_: modified buffer, dirty buffer

**Neovim pane**:
A terminal neopane shows Neovim in, attached to it: one a terminal provider made (a Rex or tmux pane, a Ghostty window), or any terminal attached by hand. A window pane or a float group pane.
_Avoid_: client, UI (panes don't attach as Neovim UIs), a neopane (that's the tool)

**Requested pane**:
A Neovim pane the daemon asked the terminal provider for, for a window or a float group.
_Avoid_: provided pane

**Self-attached pane**:
A Neovim pane whose terminal attached without being asked for, such as the one `neopane` was run in. The layout provider may still find it, as Rex finds the pane `neopane` was run in. A pane provider is only ever chosen for a self-attach.
_Avoid_: unasked pane, manual pane, launcher pane, adopted pane

**Pane id**:
The daemon's name for a Neovim pane: a number and a token, joined by `-`, such as `12-k3j9x2qpa8`. A requested pane's is given in its attach command before its terminal exists and is good for one attach; a self-attached pane gets one as it attaches.
_Avoid_: terminal id, block id

**Origin pane**:
The Neovim pane showing the window a new window came from: the one its terminal is placed beside, and the one it's drawn in, beside that window, when no terminal can be had.
_Avoid_: anchor (the anchor windows are Neovim's), source pane

**Unplaced pane**:
A requested pane left wherever its terminal provider puts new terminals, because nothing could place it beside its origin pane.

**Unshown window**:
A Neovim window no Neovim pane holds and no pane is on its way for, such as one of a tab page from `-p` not yet visited. A window in a pane's layout for a tab page that pane isn't showing is held, not unshown.
_Avoid_: hidden window, paneless window, detached window

**Layout area**:
The space a group of Neovim panes share, as the layout provider reports it, such as a Rex window, a tmux window or a sway workspace, with floating terminals over it. A Neovim pane with no live layout, or out of the pane provider's reach, is a layout area of its own; so is a requested pane the live layout still hasn't reported a second after its terminal attached with its terminal id, which until then is in no layout area. A tab page area is one when its layout provider reports it so, as a tmux window is; a tab page's Rex floating pane floats within the Rex window's. Terminals hidden while another is zoomed are reported in a layout area of their own, such as tmux's `@N~hidden`: to the daemon an ordinary layout area, though none of it is on screen.
_Avoid_: screen (Neovim's), window (Rex's, Neovim's, the OS's), workspace

**Foreign pane**:
A terminal in a layout area that isn't a Neovim pane, such as a shell or an agent in a Rex or tmux pane.
_Avoid_: shell pane, non-Neovim pane

**Current pane**:
The window pane showing Neovim's current window, or, while a float is current, the window pane last current before it. Neovim's current tab page is its window's home tab page, and Neovim's screen is the size of its layout area. Only the current pane shows the command line, messages and screen floats, except float groups with no float group pane, which stay in the pane they were last current in.
_Avoid_: focused pane (focus is the terminal's or the window manager's, and may be elsewhere), active pane

**Window pane**:
A Neovim pane showing Neovim windows: one, or several laid out as Neovim lays out splits when windows opened from it had no terminal of their own. Holding windows of several tab pages, it shows one tab page's at a time: that of the window last current in it. It also draws any float group with no float group pane that was last current in it, while that group's tab page is current.

**Pane layout**:
How a window pane's windows of one tab page share it, side by side or one above another, as Neovim lays out splits. Each window pane has one for each tab page whose windows it holds; Neovim's own layout of a tab page never holds them.
_Avoid_: split layout, window layout (Neovim's `winlayout()`), tiling

**Float group**:
A screen float with every float hanging from it, as a picker's box, input, list and preview hang together. Screen floats that can take focus and touch, as Telescope's do, make one group. Once entered, it gets a float group pane where the pane provider can open one; otherwise it is drawn in a window pane.
_Avoid_: popup, layer

**Entered**:
Of a window or float group: has been Neovim's current window since it opened (for a group, one of its floats has). Being able to take focus isn't enough: completion menus can, and are never entered.

**Float group pane**:
A Neovim pane showing one float group and nothing else of Neovim, in a floating terminal placed at the group's bounds, measured from the current pane. Never the current pane. A float group without one is drawn in a window pane.
_Avoid_: floating pane (Rex's and tmux's word for any floated pane), layer, popup

**Tabline panes**:
The window panes along the top of a layout area, which between them show Neovim's tabline, left to right, whatever tab page each shows. A window pane that is its own layout area is one; a window pane in a floating terminal never is.
_Avoid_: top-row Neovim panes, Neovim region

**Tabline width**:
The columns of the tabline panes of the current pane's layout area, summed.

**Fitted tabline**:
Neovim's tabline drawn to end at the right edge of the tabline panes of the current pane's layout area, rather than of Neovim's screen.
_Avoid_: narrowed tabline, shrunk tabline

**Home tab page**:
The tab page a window belongs to: the one it was opened in, or for a split, its origin window's. Not where Neovim keeps it: an external window sits in whichever tab page is current. Neovim's current tab page is always the current window's home tab page. A float group with a float group pane belongs to the tab page it opened in, though Neovim keeps it in the current one.
_Avoid_: owning tab, the window's tab page

**Tab page area**:
Where `open_tab_page` puts one tab page's window panes, apart from other tab pages': a tmux window or a herdr tab, each a layout area of its own, or a Rex floating pane over the Rex window. A tab page without one is drawn in the pane it opened in.
_Avoid_: tab layer, floating tab page, tab (on its own)

### Pane providers

**Pane provider**:
What gives neopane Neovim panes beyond those that attach themselves, or places and focuses the terminals in its reach: a terminal provider, a layout provider, or both, either in one, as Rex is, or paired, as Ghostty with AeroSpace. A daemon has at most one at a time, chosen for a self-attach by the user's config and kept while a terminal in its reach is attached.
_Avoid_: frontend (Neovim's word for the UI, which neopane is), host, backend, driver

**Terminal provider**:
What makes a new terminal that attaches to neopane when neopane asks for one, as Ghostty opens a window, or Rex or tmux a pane. One that is only a terminal provider can be paired with any layout provider. Without one, every Neovim pane is a self-attached pane.
_Avoid_: window provider (clashes with Rex window and Neovim window), terminal emulator (tmux makes terminals inside one)

**Attach timeout**:
How long a requested pane's terminal has to attach once its terminal provider has opened it. A window whose terminal misses it is drawn in its origin pane, and the late terminal is closed.
_Avoid_: open timeout, spawn timeout

**Layout provider**:
What finds, places, focuses, moves and resizes Neovim panes' terminals, beside one another as a split does or floating over them: AeroSpace, or Rex and tmux for their own panes. One that is only a layout provider can be paired with any terminal provider; without a terminal provider, it acts only on self-attached panes.
_Avoid_: split provider, window manager (AeroSpace is one; Rex and tmux aren't)

**Pair**:
A terminal provider and a layout provider used together as one pane provider, each half of the pair, such as Ghostty with AeroSpace. A pair opens a terminal, then places it, never both in one step.
_Avoid_: combination, combo

**Built-in provider**:
A terminal provider, layout provider or pane provider that comes with neopane, known by its name, such as `rex` or `tmux`. Rex's runs inside neopane and is never paired; the others are tables whose capabilities run neopane itself, as built-in commands, to use as they are or copy and change.
_Avoid_: default provider, bundled provider

**Built-in default**:
What chooses the pane provider with no config, and what `<name>.default` returns to one: the one built-in provider the attach's environment names, running nothing; naming several, as in a tmux started from a Rex pane, the first multiplexer whose inside proof proves, else Rex if named, else none. Its guess, if its `find` refuses it, is only logged.
_Avoid_: default provider, fallback

**Inside proof**:
A multiplexer's `<name>.inside` function, such as `inside.tmux`: whether an attach's terminal is directly one of that multiplexer's panes, and why, asking the multiplexer within 0.5 s, as tmux's does, or the process table alone, as zellij's does. That built-in's `find` runs it first, and the built-in default asks it where variables name several built-ins.
_Avoid_: detection, nesting check

**Built-in command**:
What a built-in provider's table runs for a capability: neopane itself, asked for one capability of one built-in, as `{exe} provider builtin tmux find`.
_Avoid_: helper (an attach's), shim

**Config**:
The user's own Lua file that sets up terminal and layout providers and says, for a self-attach, which pane provider to use. Neovim never reads it.
_Avoid_: Neovim's config, init.lua (Neovim's), rc

**Config child**:
The process of the daemon's own binary that runs the config for one choice and sends back what its `provider` returned, as plain data the daemon checks. It has 3 s, and once it answers, it and everything it started are killed.
_Avoid_: config process, Lua worker, sandbox (there is none)

**Capability**:
Something a pane provider can do when the daemon asks, such as open, find or focus a terminal. Each is optional: without one, that act falls back as if there were no pane provider.
_Avoid_: feature (a terminal's), operation, command

**Placeholder**:
A whole word in what a capability runs, such as `{terminal}`, that the daemon replaces with one of the act's inputs before running it; `{attach}` becomes the attach command's several words.
_Avoid_: template variable, macro

**Terminal id**:
A terminal provider's or layout provider's own name for a terminal: a Rex block id, a tmux pane id, an AeroSpace window id. A terminal from a paired pane provider has one from each.
_Avoid_: handle, block, window id, pane id

**Pane title**:
A marker neopane puts in a Neovim pane's terminal title, `<name>-` and its pane id's token, so a layout provider such as AeroSpace can find the terminal when it attaches.
_Avoid_: window title (Neovim's `'titlestring'`), tab title

**Reach**:
The terminals a pane provider counts as its own: those it made, its choosing attach's, and the self-attached ones from the choosing attach's context that its layout provider finds, if it can find any. A window opened from a pane out of reach is drawn in that pane.
_Avoid_: scope, session

**Attach context**:
Where an attach comes from, as its environment shows: the terminal app, tmux server or Rex session its terminal runs in, as far as the pane provider's context variables tell. A pane provider is chosen in one and runs its commands in it.
_Avoid_: session (Rex's, tmux's), environment (the variables, not where they point)

**Context variable**:
A variable a pane provider names as saying which attach context a terminal is in, such as `REX_SESSION`. A self-attached terminal whose value differs from the choosing attach's is in another context, so out of reach.
_Avoid_: match variable, marker

**Choosing attach**:
The self-attach the daemon's pane provider was chosen for. The config sees its environment, and the pane provider's commands run in it, even after its pane has gone.
_Avoid_: chooser, launcher, first attach (often the same attach, but not always)

**Floating terminal**:
A terminal a layout provider floats over a layout area, outside its tiling: one in a Rex or tmux floating pane, or a sway or Hyprland floating window. A float group pane's terminal is one.
_Avoid_: floating window (an OS window or a Neovim float), popup

**Live layout**:
Where a layout provider's terminals sit and which share a layout area, as it reports them: each terminal's rect as fractions of its area, any cells at the rect's edges that aren't the terminal's, and which terminal has focus.
_Avoid_: view (Rex's), snapshot

**Terminal feature**:
Something a terminal supports that Neovim can use, such as the kitty keyboard protocol, undercurl or focus reports.
_Avoid_: capability (a pane provider's)

### Rex's pane provider

Terms used only by the built-in `rex` pane provider and the Rex client (`src/providers/rex/`, `src/rex/`); the core never uses them.

**Rex window**:
The layout area of a Rex tab: the space its tiled panes share, with Rex floating panes over it.
_Avoid_: the window, terminal window

**Rex tab**:
A tab of the Rex app, holding one Rex window.
_Avoid_: tab page (that is Neovim's)

**Rex block**:
A pane of Rex's: a terminal in a Rex window or a Rex floating pane. Its id, `block:` and a UUIDv7, is its terminal id.
_Avoid_: block (on its own), Rex pane (beside Neovim pane)

**Rex floating pane**:
A pane Rex floats over a Rex window, outside its tiling. It holds a float group pane, or a tab page area's window panes, which float within the Rex window's layout area. It closes with its last Rex block.
_Avoid_: floating pane (on its own), layer (Rex's API word)

**Rex session**:
What `REX_SESSION` names: the Rex tabs one Rex server keeps for a session, and the attach context of Rex's pane provider. A terminal in another Rex session is out of its reach.
_Avoid_: session (on its own)

### zellij's pane provider

Terms used only by the built-in `zellij` pane provider (`src/providers/zellij/`); the core never uses them.

**Zellij session**:
What `ZELLIJ_SESSION_NAME` names: the zellij tabs one zellij server keeps, and the attach context of zellij's pane provider, which reaches it through the choosing attach's `ZELLIJ_SOCKET_DIR` and `HOME`. A terminal in another zellij session is out of its reach.
_Avoid_: session (on its own)

**Zellij tab**:
A tab of a zellij session: the space its tiled panes share, bars excluded, is a layout area, with zellij's floating panes over it. `open_tab_page` makes one as a tab page area.
_Avoid_: tab (on its own), tab page (that is Neovim's)
