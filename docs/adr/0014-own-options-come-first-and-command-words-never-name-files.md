# `<name>`'s own options come first, and its command words never name files

`<name> [OPTIONS] [NVIM-ARGS…]` reads its own options (`--name`, `--address`,
`--provider`, `--detach`, `--no-wait`, `-h`, `-v`) only from the front of its
command line. The first word that isn't one of them, and every word after it,
goes to Neovim as typed, a `--` included (#94). Only the first argument can be
a command word (`attach`, `list`, `status`, `kill`, `provider`, `help`,
`version`), and then it is the command even when a file of that name exists;
`./list` opens the file. So what Rex users type today, `<name> FILE` and
`<name> -O a b`, keeps working, Neovim gets its arguments unchanged, and
`<name> list` does the same in every directory.

## Considered Options

- **Our options anywhere before `--`.** Forgiving of `<name> FILE --name x`,
  but a later Neovim option spelled like one of ours would be swallowed.
  Letting options through later breaks nothing; taking them back breaks
  scripts.
- **`--` between our options and Neovim's.** It is Neovim's own end of
  options, which a file named `-x` needs.
- **A file of the same name winning over a command word.** `<name> list` would
  act differently per directory, and a stray file would leave a script waiting
  in Neovim.
- **Neovim's arguments only behind a command word**, as `<name> start -O a b`.
  Breaks what every Rex user types.
- **A parsing crate.** clap brings its own help, errors and exit codes; lexopt
  swallows the `--` Neovim needs.

## Consequences

- One of our options after Neovim's arguments have begun is a usage error
  saying where it goes; if Neovim ever adds an option of the same name,
  dropping that one check lets it through.
- The CLI reads Neovim's arguments by Neovim's own rules (letters combined,
  values attached or not, `-S` and `-q` taking theirs only sometimes) without
  changing them, to refuse what can't work under a daemon (`--embed`,
  `--headless`, `--listen`, `--remote…`, help and version, `-l`, `-es`, `-V`
  or `-r` without a file, `-`), to find the file for the unsaved-file
  question, and to hold a running daemon to `[+LINE] FILE`. Anything else
  Neovim refuses fails the start with Neovim's own message.
- Only `-h` and `-v` are short, with the meanings Neovim gives them.
- Command words may be added; `detach` is held for one. Scripts open files as
  `./NAME`.

Research: `docs/research/cli-facts.md` on branch `research/cli-facts`.
