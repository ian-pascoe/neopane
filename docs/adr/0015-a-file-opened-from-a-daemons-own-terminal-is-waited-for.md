# A file opened from a daemon's own terminal is waited for, so `EDITOR=<name>` works everywhere

Amended by #123 in implementation: `--no-wait` exits 0 once Neovim has the
file's window, or 1 after 10 s, which a Neovim held at a prompt (E325) would
otherwise make forever. The window splits off one showing the caller's
`:terminal`, as the `open` hello's new `tty` names it. Neovim's
`<name>_done {buf, by_pane}` note says whether the buffer's last window went
because its pane did: only the daemon knows a pane hung up, only Neovim knows
which window held the buffer last, and the daemon's own `<name>_unsaved` list
then says whether that was with unsaved changes. Neovim exiting is never
done: the daemon goes with it, so the file "ended before it was done".

In a daemon's own `:terminal`, `<name> FILE` opens FILE in a new window of
that daemon's Neovim and returns only once the file is done: no window shows
its buffer any more, or the buffer is unloaded or deleted (#94). `--no-wait`
returns at once. Outside any daemon, `<name> FILE` starts a daemon for the
file and returns when its Neovim quits, with Neovim's exit status, as `nvim`
does. So one setting, `EDITOR=<name>`, serves `git commit` in a Rex pane, in a
plain terminal and in a `:terminal`, with no flag to put a space in the value.

## Considered Options

- **Return at once, as nvr does, with `--wait` for editors.** `EDITOR="<name>
  --wait"` has a space, which macOS `crontab -e` and anything that runs
  `$EDITOR` without a shell can't take, and a bare `EDITOR=<name>` would hand
  `git commit` in a `:terminal` an unedited message.
- **A nested daemon in the `:terminal`, as `neorex` gives today.** A second
  Neovim, cut off from the first one's buffers and registers.
- **Done when the buffer is deleted, as nvr has it.** With `'hidden'`,
  Neovim's default, `:wq` leaves the buffer loaded, which is why nvr's users
  set `bufhidden=delete`.
- **A `--wait` on `attach` too.** An attach with a file already holds its
  terminal until its pane ends.

## Consequences

- `:wq`, `:q`, `ZZ` and `:bd` finish a waited file, and so does its window
  switching to another buffer; `<name>` exits 0. If its last window went
  because its pane's terminal closed while the buffer had unsaved changes, it
  exits 1, as `nvim` hung up would, so git doesn't take an unsaved message.
- `:cq` in a daemon started for the file exits with its status and aborts `git
  commit` or `git rebase -i`; in a shared daemon it quits every pane, as plain
  Neovim would. Aborting one waited file is left for later; git's own ways
  work meanwhile.
- Inside needs proof: `$<NAME>` names a running daemon and this terminal is
  one of its Neovim's `:terminal`s, or the process has no terminal at all. A
  tmux server or terminal app started from a `:terminal` copies `$<NAME>` into
  terminals that aren't Neovim's.
- The `open` control request gains `wait`. The reply to a waited open comes
  when the file is done and says `waited`, so an older daemon that ignored
  `wait` is caught rather than taken for done.

Research: `docs/research/cli-facts.md` on branch `research/cli-facts`.
