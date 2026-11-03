# neorex

neorex shows one Neovim's windows as panes of Rex, a terminal with tiled and floating panes.

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

**Neovim pane**:
A Rex pane neorex runs: a window pane or a floating pane.

**Window pane**:
A tiled Neovim pane showing one Neovim window.

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
