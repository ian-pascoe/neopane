# neorex becomes neopane, with no aliases for its old names

Once panes attach to a core and Rex is one pane provider among several, the
tool is no longer Rex-only, and the name neorex belongs to the project it
began as: alexrlocke/neorex, which is still active under that name, with its
own `neorex` binary, `g:neorex_*` variables and colorscheme. So neorex
becomes **neopane** (shorthand `npane`), and every user-visible name changes
at once with no aliases, since nothing has been published and its users are a
handful (#85). The name must be letters only, so every form of it needs no
escaping; must read as nothing Neovim ships; and must name no pane provider.
"neo-" is how third-party Neovim projects mark themselves, and panes are what
users get.

## Considered Options

- **Keep neorex.** It collides with the original and names one pane provider.
- **`nvimd` or `neovimd`.** Both read as Neovim's own daemon, which invites
  `nvim --remote-ui` at a daemon's `nvim.sock` and breaks the shared screen
  (ADR 0001). The `-d` convention also marks the server you never hand files
  to, the name repeats the glossary's **Daemon**, and ahdinosaur/nvimd ("a
  long-running neovim server") already exists.
- **A `-mux` or `-plex` name.** `nmux`, `nvmux`, `neomux`, `nvimux` and
  `neoplex` are all Neovim multiplexers already.
- **`oriel`.** Shorter, and clashes with no glossary term, but it says
  nothing about Neovim, is hard to search for, and its crate and PyPI names
  are taken.
- **`panvim`.** Searches for it find panvimdoc.
- **Aliases for a while.** There's no release to end them at, and keeping
  `g:neorex` or `_G.neorex` would go on occupying the original's names.

## Consequences

- Every surface comes from the one name: the `neopane` binary and its `npane`
  symlink, the crate, the repo (renamed in place; `ian-pascoe/neorex` is never
  recreated, which would kill the redirect), `g:neopane`,
  `g:neopane_tabline_width`, `NeopaneTabLineSel` and `NeopaneTabLine`,
  `_G.neopane`, the augroup and namespace, `b:neopane_anchor`, the
  `neopane_*` notes, `$NEOPANE`, `$NEOPANE_TMPDIR`, `/tmp/neopane-<uid>/`, and
  the Rex block label and client name. From the first attach-core commit,
  they all come from one constant.
- `g:neopane` is `1`, set before the config loads, and only says "running
  under neopane": it is never a config table. Configs detect neopane by it,
  never by matching a socket path.
- The rename is the next merge after Rex runs as a pane provider on the
  attach core, in its own PR so a Rex regression bisects to behaviour, not to
  naming, and no tag comes between them. The old names never ship, so ADR
  0004's cross-version rule starts at the first tag and the rename is no
  exception to it.
- Users change their `vim.g.neorex` checks by hand, from a "Renamed from
  neorex" table in the README. `just install` removes our old `nrex` symlink
  and only warns about a `neorex` beside it, which may be the original's;
  both go at the first published release.
- The repo isn't a GitHub fork, so the README is the only link to the
  original: the credit sits under the tagline as well as in Credits, and
  LICENSE keeps Alex Locke's line.
- Nothing presents neopane as part of Neovim or its brand: it's "for
  Neovim", never "Neovim's".

Research: `docs/research/new-name.md` on branch `research/new-name`.
