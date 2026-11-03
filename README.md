# neorex

A Rex-Native Neovim frontend built in Rust.

One Neovim runs in the background behind all your panes, so buffers,
registers, undo and language servers are shared. `:split` and `:vsplit` open
real Rex panes where Neovim would put the window, and closing a pane in Rex
closes its window. Focusable floats, such as pickers, open in Rex floating
panes, along with any floats attached to them. The completion menu,
unfocusable floats like notifications, and statuslines are drawn inside the
pane they belong to.

![neorex demo](assets/demo.gif)

## Status

Very early and rough: expect bugs and missing features.

neorex has been built with heavy use of AI, first in Zig and now in Rust.
Much of the code still needs cleanup and better design, which is happening
incrementally. Contributions are welcome.

## Requirements

- macOS, or Linux: it builds and passes its tests there, but hasn't been tried live yet (#43)
- [Rex](https://www.superlogical.com/rex)
- A recent Neovim, 0.12 or later.
- Rust (stable, 2024 edition) to build
- Zig 0.17, only for the golden tests: they build Ghostty

## Build and run

With [just](https://github.com/casey/just) (`just` lists the recipes):

```sh
just build
just test
just golden   # also the golden tests; downloads and builds Ghostty once
```

or with Cargo alone:

```sh
cargo build
cargo test
scripts/golden.sh --test golden
```

The golden tests check every frame neorex draws against
[Ghostty](https://github.com/ghostty-org/ghostty)'s terminal emulator, pinned
to one commit. They're behind the `golden` feature, so a plain build never
builds Ghostty.

To install it, optimized, as `~/.local/bin/neorex`, with `nrex` linked to
it as a shorthand:

```sh
just install
just prefix=/usr/local install
```

It replaces the binary rather than writing over it, which on macOS gets a
new build killed at launch.

Run it in a Rex pane, with any Neovim arguments:

```sh
target/debug/neorex some/file.txt
```

Neovim starts with `g:neorex` set, before your config loads, so the config
can tell it's running under neorex:

```lua
{ 'sphamba/smear-cursor.nvim', cond = vim.g.neorex == nil },
```

The tabline shows across the panes along the top of the Rex window. When
another pane, such as a shell, takes part of the top, those panes are narrower
than Neovim's screen, and neorex fits the tabline to them: while a `%!`
`'tabline'` is drawn, `vim.o.columns` reads as their width, so a tabline whose
Lua lays out to it, such as bufferline's, fits. Other tablines, such as
Neovim's own and lualine's, still lay out across the whole screen; they can use
`g:neorex_tabline_width`, set to that width while it's narrower than the
screen:

```lua
local width = vim.g.neorex_tabline_width or vim.o.columns
```

`neorex help` lists the debug commands, which run each layer on its own:
keys, Neovim's redraw events, the renderer, and pane hand-off.

## How it runs

Running `neorex` in a Rex pane starts a daemon in the background, which runs
one headless Neovim and gives each of its windows a Rex pane. The pane you ran
it in becomes the first window's. It takes its bearings from the environment
Rex sets in every pane:

- `REX_SERVER`: the URL of the Rex server to connect to. Required.
- `REX_SESSION`: the Rex session to open panes in. Required.
- `REX_BLOCK`: the id of the pane `neorex` was run in. Optional: the daemon
  uses it as the first window's pane, and logs a note if it isn't a block id.

If stdin isn't a terminal, `neorex` prints `error: NotATerminal` and exits. In
a terminal but without `REX_SERVER` or `REX_SESSION`, it prints
`neorex: run it in a Rex pane`, then `error: NotInRex`, and exits. The
`debug-rex` command reads the same three variables.

The daemon keeps its files (Neovim's socket, its own socket, and the log) in a
private directory, `neorex-<launcher pid>.XXXXXX`, in `$XDG_RUNTIME_DIR`, else
`$TMPDIR`, else `/tmp`; each is used only when it is an absolute path. It logs
to the file `log` there. On a clean exit (Neovim quits or crashes, the last
pane closes, or a signal) the daemon removes the log and the directory. When
the daemon itself fails, they stay, and if it failed to start, neorex prints
the log's path. `neorex help` says the same.

To try a build without it grabbing the focus in your own Rex session, which
neorex does as it works, make a session of your own with `rex new`, and run it
there, or in a terminal started with `REX_SESSION` set to that session's id
(`REX_SERVER` stays as Rex set it). A terminal started from one of your own
panes inherits that pane's `REX_BLOCK`, which the daemon would use as the first
window's pane, in your session: unset `REX_BLOCK`, or set it to a block in the
new session.

## Troubleshooting

- "run it in a Rex pane": `REX_SERVER` or `REX_SESSION` is missing. Start
  `neorex` from a Rex pane, or set both (see above) to a session you made.
- "the daemon didn't start; see <path>", or a failure line in the pane: read
  the log at that path.
- To find a log left by a daemon failure, look in the directories named
  `neorex-*` in `$XDG_RUNTIME_DIR`, `$TMPDIR` or `/tmp`.
- While it runs, Neovim's own messages go to the same log, since the
  daemon's stderr is Neovim's.

## Current limitations

- The `<C-w>` resize, swap and move commands do nothing. Use Rex's own pane
  controls instead. The exception is resizing a window in a tab page's
  floating pane, below: that resizes its Rex pane.
- `<C-w>h/j/k/l` (also `<C-w><C-h>`, `<C-w><BS>` and `<C-w><Left>` and the like,
  in Normal and Visual mode) move to the neighbouring pane in Rex, and stay
  put at an edge. A count repeats the move, up to 20 times. Mappings that
  remap them work, such as LazyVim's `<C-h>`; a `noremap` mapping to `<C-w>h`
  (kickstart.nvim's) skips them, and so do `:wincmd h` and `vim.cmd.wincmd`
  from a plugin: those step through the panes like `<C-w>w`. The move happens
  after the keys typed with it: `<C-w>lZZ` in a mapping or macro runs `ZZ` in
  the old window.
  - Rex servers whose `session.focus_direction` has no `wrap` option wrap
    around at an edge. Ones without the method do nothing for these keys,
    and log the refusal.
  - Rex looks at every pane in its window, so a move can enter an open
    floating pane, such as a tab page's, and leave it.
- Plugins that arrange windows (sidebars, layout managers) don't get the
  placement they expect, except in a tab page's floating pane, below. A
  window such a plugin moves back into Neovim's own layout goes back to its
  pane. edgy.nvim works without turning it off:
  neorex tells it to leave the windows in panes alone (`vim.w.edgy_disable`),
  so its edges don't dock anything. A plugin that keeps moving a window back
  as fast as neorex puts it in its pane (5 times in 2 seconds, with nothing
  typed and no pane resized) wins for a while: that window stays in Neovim's
  layout, its pane stays blank, and you still type into it. neorex tries
  again after 2 seconds, then 4, up to a minute. Turn such a plugin off with
  `cond = vim.g.neorex == nil`.
- Neovim tab pages aren't Rex tabs. A tab page opened after startup, as
  `:tabnew`, Neogit and Diffview open, shows in a Rex floating pane and
  closes with its last window. Its windows' panes are placed as Neovim's
  layout has them, at about their sizes, so Diffview's file panel is on the
  left. They follow the layout as windows come and go, and a window Neovim
  resizes, such as with `<C-w>|`, resizes its pane. Moving windows once they
  show in panes, as with `<C-w>H`, still does nothing.
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
  A pane's own `setlocal statusline` is replaced by that line meanwhile, and
  comes back when the pane takes focus, or when `laststatus` is set to
  another value.

## Plans

Much of neorex will be split out into standalone crates:

- a Neovim RPC client (msgpack, message framing, requests)
- a Rex API client (WebSocket and the control protocol)
- terminal key input (the kitty keyboard protocol)

neorex should then be easier to extend to other editors that provide similar processes for running external windows.

## Credits

neorex was created by [Alex Locke](https://github.com/alexrlocke), in Zig, at
[alexrlocke/neorex](https://github.com/alexrlocke/neorex). This repository
started as a fork of it and continues it independently, rewritten in Rust.
The commit history from the original is kept.

## License

MIT, as the original. See [LICENSE](LICENSE).
