# The Herdr app's pane provider is a built-in of its own, named herdr-app

The Herdr desktop app gets a built-in pane provider of its own,
`BuiltinTable::HerdrApp` in `src/providers/herdr_app/`, named `herdr-app` as
a string (its table's `name`, `provider show herdr-app`, `{exe} provider
builtin herdr-app CAPABILITY`) and `herdr_app` as a Lua identifier
(`<name>.builtin.herdr_app()`, `<name>.inside.herdr_app`), built in with no
config-first stage (SPI-1213). The app has nothing of herdr, the terminal
multiplexer, but a name: its panes show terminals an hc core runs, which
speaks JSON-RPC over its own socket, and its window acts through the
`herdr-app` command; herdr's pane provider speaks herdr's socket API.

Its reach is the hc core, by its socket, with the app's home: context
variables `HC_TERMINAL_SOCKET` and `HERDR_APP_HOME`, find variable
`HC_TERMINAL_ID`. A Herdr app space is no boundary. The built-in default
names it when `HC_TERMINAL_ID` and `HC_TERMINAL_SOCKET` are set and not
empty, never by `TERM_PROGRAM`, which the app's terminals set to `ghostty`
as plain Ghostty does; it is proved last of the multiplexers: tmux, herdr,
zellij, herdr-app, then Rex.

Reads go over the core's socket (`hello`, `terminal.get`, `terminal.kill`,
`doc.get herdr-app`), window acts through `herdr-app`, found on `PATH`, else
at `~/.local/bin/herdr-app`, in modules of their own. Its terminal ids are
hc terminal addresses, such as `t7`; the core's doc is the only map from a
terminal to its Herdr app pane.

## Considered Options

- **A mode of herdr's built-in.** Nothing is shared: not the socket, its
  protocol, the ids, nor the variables. Every capability would branch on
  which it is, and `provider show herdr` would list a table that is two.
- **Named by `TERM_PROGRAM`, as tmux and herdr are.** The app's terminals
  say `ghostty`, so it would claim every Ghostty window, or none.
- **Acting on the window through the core's doc (`doc.set`).** The doc is
  the app's to write; `herdr-app do` is its documented way to act, and
  the core alone has no focus or zoom: those are a Herdr app window's.
- **A version floor**, as tmux's 3.2 and herdr's protocol 22. Both the core
  and the app move fast, and `hello` lists its methods: a capability needs
  what it calls, not a version.
- **A config-first stage**, a table users write before it's built in. The
  facts were settled in a spike against current versions (SPI-1214), with
  fixtures the unit tests replay.

## Consequences

- `find` and `<name>.inside.herdr_app` prove the attach: `hello` in
  generation 1; `terminal.get` live, its process on the attach's tty; one
  block of the doc showing it; `herdr-app status` answering. A remote
  terminal, `machine/t7`, is refused. Unprovable, `find` trusts
  `HC_TERMINAL_ID` as herdr's trusts `HERDR_PANE_ID`, and `inside` says no.
- Feature detection, no floor: a capability whose method the core lacks, or
  a core of another generation, exits 69 naming it and both versions; `find`
  refuses instead, as exit 69 would accept the terminal. `provider check`
  prints both versions.
- `close` is `terminal.kill`, done for a terminal finished or not found.
- The GLOSSARY's herdr section is the multiplexer's; the app's terms (hc
  core, hc terminal, Herdr app pane, tab, space and window) are their own.
