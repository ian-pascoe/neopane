# neorex

neorex runs Neovim in the background and shows its windows in the terminals attached to it, which a pane provider, such as Rex, can open and place.

## Language

### Rex

**Rex window**:
The area the tiled panes of the current Rex tab share, with floating panes floating over it.
_Avoid_: the window, terminal window

**Rex tab**:
A tab of the Rex app, holding one Rex window.
_Avoid_: tab page (that is Neovim's)

**Foreign pane**:
A Rex pane neorex doesn't run, such as a shell or an agent.
_Avoid_: shell pane, non-Neovim pane

### Neovim

**Neovim's screen**:
The area Neovim lays its editor out on, `'columns'` by `'lines'` cells, drawn as grid 1.
_Avoid_: editor, editor size, default grid

**Tab page**:
Neovim's tab page.
_Avoid_: tab (on its own)

**Built-in tabline**:
The tabline Neovim shows when `'tabline'` is empty: a label per tab page, then a close button.
_Avoid_: default tabline, Neovim's own tabline

### neorex

**Daemon**:
What a user starts, attaches terminals to, lists and kills: one Neovim, run in the background, showing its windows in the Neovim panes attached to it. A user may run several.
_Avoid_: session (Rex's, tmux's and `:mksession`'s), server (Neovim's `--listen`), instance, workspace

**Daemon name**:
What a daemon is found by: unique among a user's running daemons, and by default the name of the directory it was started in.
_Avoid_: session name

**Daemon address**:
Where a daemon takes attaches: a Unix socket path, or a named pipe on Windows. Passed whole, never taken apart.
_Avoid_: socket, socket path

**Unattached daemon**:
A daemon with no Neovim pane attached or on its way, whether it hasn't had one yet or its last pane went.
_Avoid_: detached daemon (kept for a deliberate detach), headless, orphan, paneless

**First attach**:
The attach that takes a daemon from unattached to attached: the one `neorex` makes when it starts a daemon, or the first after its last pane went.
_Avoid_: initial attach

**Unsaved changes**:
A buffer `:qa` would refuse to quit over: modified and writable. A running `:terminal` job is never one.
_Avoid_: modified buffer, dirty buffer

**Neovim pane**:
A terminal neorex shows Neovim in, attached to it: one a terminal provider made (a Rex or tmux pane, a Ghostty window), or any terminal attached by hand. A window pane or a floating pane.
_Avoid_: client, UI (panes don't attach as Neovim UIs)

**Requested pane**:
A Neovim pane the daemon asked the terminal provider for, for a window or a float group.
_Avoid_: provided pane

**Self-attached pane**:
A Neovim pane whose terminal attached without being asked for, such as the one `neorex` was run in. The layout provider may still know it, as Rex knows the pane `neorex` was run in.
_Avoid_: unasked pane, manual pane, launcher pane, adopted pane

**Pane id**:
The daemon's name for a Neovim pane, given to a requested pane before its terminal exists.
_Avoid_: terminal id, block id

**Origin pane**:
The Neovim pane showing the window a new window came from: the one its terminal is placed beside, and the one it's drawn in when no terminal can be had.
_Avoid_: anchor (the anchor windows are Neovim's), source pane

**Unplaced pane**:
A requested pane left wherever its terminal provider puts new terminals, because nothing could place it beside its origin pane.

**Unshown window**:
A Neovim window no Neovim pane shows and no pane is on its way for.
_Avoid_: hidden window, paneless window, detached window

**Window pane**:
A Neovim pane showing one Neovim window.

**Floating pane**:
A Neovim pane in a Rex floating pane, showing a group of Neovim floats or a tab page's windows.

**Tabline panes**:
The window panes along the top of the Rex window, which between them show Neovim's tabline, left to right.
_Avoid_: top-row Neovim panes, Neovim region

**Tabline width**:
The tabline panes' columns, summed.

**Fitted tabline**:
Neovim's tabline drawn to end at the right edge of the tabline panes rather than of Neovim's screen.
_Avoid_: narrowed tabline, shrunk tabline

**Home tab page**:
The tab page a window belongs to: the one it was opened in, or for a split, its origin window's. Not where Neovim keeps it: an external window sits in whichever tab page is current.
_Avoid_: owning tab, the window's tab page

### Pane providers

**Pane provider**:
What gives neorex Neovim panes beyond those that attach themselves: a terminal provider, with an optional layout provider to place them. Rex is both in one; Ghostty with AeroSpace is the two apart.
_Avoid_: frontend (Neovim's word for the UI, which neorex is), host, backend, driver

**Terminal provider**:
What makes a new terminal that attaches to neorex when neorex asks for one, as Ghostty opens a window, or Rex or tmux a pane. Without one, every Neovim pane is a self-attached pane.
_Avoid_: window provider (clashes with Rex window and Neovim window), terminal emulator (tmux makes terminals inside one)

**Layout provider**:
What finds, places, focuses, moves and resizes Neovim panes' terminals, beside one another as a split does or floating over them: AeroSpace, or Rex and tmux for their own panes.
_Avoid_: split provider, window manager (AeroSpace is one; Rex and tmux aren't)

**Capability**:
Something a pane provider can do when the daemon asks, such as open, find or focus a terminal. Opening a terminal is the only one every pane provider has.
_Avoid_: feature (a terminal's), operation, command

**Terminal id**:
A pane provider's own name for a terminal: a Rex block id, a tmux pane id, an AeroSpace window id.
_Avoid_: handle, block, window id, pane id

**Reach**:
The terminals a pane provider counts as its own: those it made, the one whose attach chose it, and the self-attached ones its layout provider finds, or every one if it can't find any. A window opened from a pane out of reach is drawn in that pane.
_Avoid_: scope, context, session

**Live layout**:
Where a layout provider's terminals sit and which share an area, as it reports them when asked.
_Avoid_: view (Rex's), snapshot

**Terminal feature**:
Something a terminal supports that Neovim can use, such as the kitty keyboard protocol, undercurl or focus reports.
_Avoid_: capability (a pane provider's)
