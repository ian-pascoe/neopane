# A daemon outlives its last pane only while Neovim has unsaved changes

When a daemon's last Neovim pane goes, it ends, as neorex always has, unless
Neovim has unsaved changes: buffers `:qa` would refuse to quit over
(`getbufinfo(b)[1].changed`). Then it stays, unattached, until an attach comes
or the changes are gone (#84). An accidentally closed terminal becomes an
`attach` instead of a swap recovery, while a Rex user who closes the last pane
with nothing unsaved sees no change, and no Neovim is left running unseen
unless it holds work nobody threw away.

## Considered Options

- **Always outlive the last pane, as a tmux server does.** Every closed
  terminal would leave a Neovim, its language servers and its swap files
  running unseen, and change how every Rex user's Neovim ends.
- **Always end (as before).** Unsaved changes survive only as swap files.
- **An idle timeout.** Ends kept work at an arbitrary moment.
- **Count running `:terminal` jobs as unsaved.** An idle shell can't be told
  from a build, so every `:terminal` would keep its daemon: the tmux model
  again. A running job dies when the daemon ends, as under a plain `:qa`.

## Consequences

- "The last pane goes" means a daemon that has had a pane is left with no pane
  showing a window: a requested pane still on its way counts, panes showing
  only floats don't, and the cause doesn't matter. A launcher attach that never
  arrives is a failed start instead.
- Each pane that goes closes its windows, so an unattached daemon keeps one
  window, the last current one that isn't a float, and a reattach opens no
  burst of terminals. Keeping a whole layout is for a deliberate `detach`,
  which, like starting without attaching, sets an "asked to stay" flag.
- On going unattached the daemon cancels pending pane requests, sends
  FocusLost so autosave can run, waits for Neovim's next SafeState (about 1 s
  at most), and decides from a `neorex_unsaved` note `windows.lua` keeps
  current, not from a request, which would wait behind a hit-enter prompt.
- The daemon never quits Neovim with `:qa`, `:qa!` or `:confirm`: with
  `'confirm'` set, `:qa` from an attached UI waits at a prompt nobody sees.
  It ends Neovim by closing its `--embed` channel, so Neovim runs
  `preserve_exit()`: VimLeavePre and VimLeave run, ShaDa is written, swap files
  are kept for changed buffers and deleted for the rest. A change made in the
  last moment goes to swap, never away.
- Neovim runs as `nvim --embed --headless --listen …` with the daemon's RPC on
  stdio, so a daemon that dies, even by SIGKILL, takes Neovim with it, its
  swap kept, on every platform. Every fd the daemon holds is close-on-exec,
  so nothing it spawns keeps Neovim alive. `:detach` closes that channel and
  nothing can veto it over RPC: the daemon treats it as Neovim exiting and
  sends Neovim SIGHUP. `windows.lua` refuses a typed `:detach`, but a
  plugin's `vim.cmd('detach')` gets past that, so the SIGHUP rule isn't
  redundant with it.
- A daemon that goes unattached drops its pane provider, and the next
  self-attach chooses again from its own context (ADR 0007), so a resumed
  daemon never opens windows where nobody is looking.

Research: `docs/research/daemon-lifetime.md` on branch
`research/daemon-lifetime`.
