# neorex

A Rex-Native Neovim frontend built in Zig.

One Neovim runs in the background behind all your panes, so buffers,
registers, undo and language servers are shared. `:split` and `:vsplit` open
real Rex panes where Neovim would put the window, and closing a pane in Rex
closes its window. Focusable floats, such as pickers, open in Rex floating
panes, along with any floats attached to them. The completion menu,
unfocusable floats like notifications, and statuslines are drawn inside the
pane they belong to.

![neorex demo](assets/demo.gif)

## Status

Very early and rough, bugs / missing features are to be expected.

AI has been used heavily in building neorex due to limits on my available time. A lot of the code needs large improvements, cleanup, better design etc. I will be doing this incrementally - any contributions are welcome.

## Requirements

- macOS (it uses kqueue)
- [Rex](https://www.superlogical.com/rex)
- A recent Neovim, 0.12 or later.
- Zig 0.17 to build

## Build and run

With [just](https://github.com/casey/just) (`just` lists the recipes):

```sh
just build
just test
just golden   # also the golden tests; downloads Ghostty once
```

or with Zig alone:

```sh
zig build
zig build test
zig build test -Dgolden
```

The golden tests check every frame neorex draws against
[Ghostty](https://github.com/ghostty-org/ghostty)'s terminal emulator, pinned
to one commit.

To install it, optimized, as `~/.local/bin/neorex`, with `nrex` linked to
it as a shorthand:

```sh
just install
just prefix=/usr/local optimize=ReleaseFast install
```

It replaces the binary rather than writing over it, which on macOS gets a
new build killed at launch. `zig build -Doptimize=ReleaseSafe --prefix ~/.local`
does the same.

Run it in a Rex pane, with any Neovim arguments:

```sh
zig-out/bin/neorex some/file.txt
```

Neovim starts with `g:neorex` set, before your config loads, so the config
can tell it's running under neorex:

```lua
{ 'sphamba/smear-cursor.nvim', cond = vim.g.neorex == nil },
```

`neorex help` lists the debug commands, which run each layer on its own:
keys, Neovim's redraw events, the renderer, and pane hand-off.

## Current limitations

- The `<C-w>` resize, swap and move commands do nothing. Use Rex's own pane
  controls instead.
- Plugins that arrange windows (sidebars, layout managers) don't get the
  placement they expect.
- Neovim tab pages aren't Rex tabs. A tab page opened after startup, as
  `:tabnew`, Neogit and Diffview open, shows in a Rex floating pane, its
  windows split inside it, and closes with its last window.
- Plugins that draw cursor effects with editor floats, such as
  smear-cursor.nvim, draw them in the wrong place: Neovim can't tell them
  where a pane's window is.
- No external command line or messages: long messages still show
  "Press ENTER".
- No global statusline: neorex treats `laststatus=3` as `2`, so each pane
  shows a statusline, the current window's: with lualine's
  `globalstatus = true`, the same line in every pane. Set `globalstatus`
  explicitly: lualine skips neorex's windows without it, and its default,
  `vim.go.laststatus == 3`, is false once neorex has turned `laststatus=3`
  into `2`.

## Plans

Much of neorex will be split out into standalone Zig modules, and later into
their own packages:

- a Neovim RPC client (msgpack, message framing, requests)
- a Rex API client (WebSocket and the control protocol)
- terminal key input (the kitty keyboard protocol)

neorex should then be easier to extend to other editors that provide similar processes for running external windows.

## License

MIT
